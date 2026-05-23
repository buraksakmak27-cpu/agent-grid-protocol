# -*- coding: utf-8 -*-
"""
Agent Grid Python SDK
=====================
Yapay Zeka API Kota Borsasi icin resmi Python istemcisi.

Kurulum:
    pip install requests

Hizli Baslangic:
    from agent_grid_client import AgentGrid

    grid = AgentGrid()
    response = grid.chat_completion(
        model="gpt-4o",
        messages=[{"role": "user", "content": "Merhaba!"}]
    )
    print(response["choices"][0]["message"]["content"])
"""

import requests


class AgentGrid:
    """
    Agent Grid Borsa Istemcisi.

    Yerel veya uzak bir Token Borsasi sunucusuna baglananarak
    OpenAI uyumlu chat completion istekleri gonderir.
    Sunucu, emir defterindeki en ucuz kotayi otomatik eslestirir.

    Args:
        base_url (str): Sunucu adresi. Varsayilan: http://127.0.0.1:3000/v1
        timeout  (int): Istek zaman asimi (saniye). Varsayilan: 30
    """

    def __init__(self, base_url: str = "http://127.0.0.1:3000/v1", api_key: str = "test-key-123", timeout: int = 30):
        self.base_url = base_url.rstrip("/")
        self.api_key  = api_key
        self.timeout  = timeout
        self._session = requests.Session()
        self._session.headers.update({
            "Content-Type": "application/json",
            "x-api-key": self.api_key
        })

    # ------------------------------------------------------------------
    # Ana yontemler
    # ------------------------------------------------------------------

    def chat_completion(self, model: str, messages: list, **kwargs) -> dict:
        """
        Borsaya chat completion istegi gonderir.

        Sunucu, emir defterinden verilen model icin en ucuz kotayi bulur,
        1000 token duserek tuketir ve OpenAI uyumlu bir yanit dondurur.

        Args:
            model    (str):  Kullanilacak model adi. Ornek: "gpt-4o"
            messages (list): OpenAI formatinda mesaj listesi.
                             [{"role": "user", "content": "..."}]
            **kwargs:        Ekstra parametreler (temperature, max_tokens vb.)
                             oldugu gibi sunucuya iletilir.

        Returns:
            dict: OpenAI uyumlu yanit sozlugu.

        Raises:
            AgentGridError:      Sunucu hata kodu dondurdugunde.
            AgentGridConnError:  Sunucuya ulasilamadiginda.
        """
        payload = {"model": model, "messages": messages, **kwargs}
        url     = f"{self.base_url}/chat/completions"

        try:
            resp = self._session.post(url, json=payload, timeout=self.timeout)
        except requests.exceptions.ConnectionError as exc:
            raise AgentGridConnError(
                f"Sunucuya baglanılamadı: {self.base_url}\n"
                f"  'cargo run' komutuyla sunucunun calistigından emin olun.\n"
                f"  Detay: {exc}"
            ) from exc
        except requests.exceptions.Timeout as exc:
            raise AgentGridConnError(
                f"Istek zaman asimina ugradi ({self.timeout}s): {url}"
            ) from exc

        if not resp.ok:
            raise AgentGridError(
                status_code=resp.status_code,
                message=resp.text,
            )

        return resp.json()

    def list_orders(self) -> dict:
        """
        Borsadaki aktif emir defterini dondurur.

        Returns:
            dict: {"total_orders": int, "asks": [...]}
        """
        url = f"{self.base_url.replace('/v1', '')}/book"
        try:
            resp = self._session.get(url, timeout=self.timeout)
        except requests.exceptions.ConnectionError as exc:
            raise AgentGridConnError(str(exc)) from exc

        if not resp.ok:
            raise AgentGridError(status_code=resp.status_code, message=resp.text)

        return resp.json()

    def add_order(self, provider: str, model: str, token_amount: int, price_per_1k: float) -> dict:
        """
        Borsaya yeni kota satis emri ekler.

        Args:
            provider     (str):   "OpenAI" veya "Anthropic"
            model        (str):   Model adi. Ornek: "gpt-4o"
            token_amount (int):   Satislik token miktari. Ornek: 10000
            price_per_1k (float): 1000 token basina USD fiyati. Ornek: 0.0015

        Returns:
            dict: Olusturulan emir bilgileri.
        """
        url     = f"{self.base_url.replace('/v1', '')}/order"
        payload = {
            "provider":     provider,
            "model":        model,
            "token_amount": token_amount,
            "price_per_1k": price_per_1k,
        }
        try:
            resp = self._session.post(url, json=payload, timeout=self.timeout)
        except requests.exceptions.ConnectionError as exc:
            raise AgentGridConnError(str(exc)) from exc

        if not resp.ok:
            raise AgentGridError(status_code=resp.status_code, message=resp.text)

        return resp.json()

    # ------------------------------------------------------------------
    # Cüzdan ve Ödeme İşlemleri
    # ------------------------------------------------------------------

    def get_balance(self) -> dict:
        """
        Kullanicinin guncel cüzdan bakiyesini getirir.

        Returns:
            dict: Bakiye bilgileri (usd_balance, crypto_balance, total_balance)
        """
        # base_url sonundaki /v1 kısmını kaldırıp /api/wallet'e istek atacağız
        api_url = self.base_url.replace("/v1", "") + "/api/wallet"
        try:
            resp = self._session.get(api_url, timeout=self.timeout)
        except requests.exceptions.ConnectionError as exc:
            raise AgentGridConnError(str(exc)) from exc

        if not resp.ok:
            raise AgentGridError(status_code=resp.status_code, message=resp.text)

        return resp.json()

    def deposit_mock_stripe(self, amount: float) -> dict:
        """
        Stripe / Lemon Squeezy uzerinden basarili bir kredi karti 
        odemesini simule eder.

        Args:
            amount (float): Yuklenecek USD miktari

        Returns:
            dict: Islem sonucu ve yeni bakiye
        """
        api_url = self.base_url.replace("/v1", "") + "/api/pay/stripe-webhook"
        payload = {
            "api_key": self.api_key,
            "amount": amount,
            "payment_intent": "pi_mock_12345"
        }
        try:
            resp = self._session.post(api_url, json=payload, timeout=self.timeout)
        except requests.exceptions.ConnectionError as exc:
            raise AgentGridConnError(str(exc)) from exc

        if not resp.ok:
            raise AgentGridError(status_code=resp.status_code, message=resp.text)

        return resp.json()

    def deposit_mock_crypto(self, tx_hash: str, amount: float) -> dict:
        """
        Solana / EVM uzerinden gonderilen USDC islemini 
        dogrulamayi simule eder.

        Args:
            tx_hash (str): Islem (TX) ozeti / hash'i
            amount (float): Gonderilen USDC miktari

        Returns:
            dict: Islem sonucu ve yeni bakiye
        """
        api_url = self.base_url.replace("/v1", "") + "/api/pay/crypto-verify"
        payload = {
            "api_key": self.api_key,
            "tx_hash": tx_hash,
            "amount": amount,
            "chain": "solana"
        }
        try:
            resp = self._session.post(api_url, json=payload, timeout=self.timeout)
        except requests.exceptions.ConnectionError as exc:
            raise AgentGridConnError(str(exc)) from exc

        if not resp.ok:
            raise AgentGridError(status_code=resp.status_code, message=resp.text)

        return resp.json()

    # ------------------------------------------------------------------
    # Yardimci yontemler
    # ------------------------------------------------------------------

    def cheapest_message(self, model: str, content: str) -> str:
        """
        Tek satirlik kolaylik yontemi.
        Verilen modele bir mesaj gonderir ve asistan yanitini string olarak dondurur.

        Args:
            model   (str): Kullanilacak model adi.
            content (str): Kullanici mesaji.

        Returns:
            str: Asistanin yanit metni.
        """
        response = self.chat_completion(
            model=model,
            messages=[{"role": "user", "content": content}]
        )
        return response["choices"][0]["message"]["content"]

    def claude_completion(self, model: str, messages: list, max_tokens: int = 1024, **kwargs) -> dict:
        """
        Borsaya Anthropic Claude mesaj istegi gonderir.

        Sunucu, emir defterinden verilen Claude modeli icin en ucuz
        Anthropic kotasini bulur, 1000 token duserek tuketir ve
        resmi Anthropic Messages API formatinda yanit dondurur.

        Args:
            model      (str):  Kullanilacak Claude modeli. Ornek: "claude-3-5-sonnet"
            messages   (list): Anthropic formatinda mesaj listesi.
                               [{"role": "user", "content": "..."}]
            max_tokens (int):  Maksimum uretilecek token sayisi. Varsayilan: 1024
            **kwargs:          Ekstra parametreler sunucuya oldugu gibi iletilir.

        Returns:
            dict: Anthropic Messages API uyumlu yanit sozlugu.
                  Metin icerigi: response["content"][0]["text"]

        Raises:
            AgentGridError:     Sunucu hata kodu dondurdugunde.
            AgentGridConnError: Sunucuya ulasilamadiginda.
        """
        payload = {"model": model, "messages": messages, "max_tokens": max_tokens, **kwargs}
        url     = f"{self.base_url}/messages"

        try:
            resp = self._session.post(url, json=payload, timeout=self.timeout)
        except requests.exceptions.ConnectionError as exc:
            raise AgentGridConnError(
                f"Sunucuya baglanılamadı: {self.base_url}\n"
                f"  'cargo run' komutuyla sunucunun calistigından emin olun.\n"
                f"  Detay: {exc}"
            ) from exc
        except requests.exceptions.Timeout as exc:
            raise AgentGridConnError(
                f"Istek zaman asimina ugradi ({self.timeout}s): {url}"
            ) from exc

        if not resp.ok:
            raise AgentGridError(status_code=resp.status_code, message=resp.text)

        return resp.json()

    def ask_claude(self, content: str, model: str = "claude-3-5-sonnet") -> str:
        """
        Tek satirlik Claude kolaylik yontemi.
        Verilen metni Claude'a gonderir ve yanit metnini string olarak dondurur.

        Args:
            content (str): Kullanici mesaji.
            model   (str): Claude modeli. Varsayilan: "claude-3-5-sonnet"

        Returns:
            str: Claude'un yanit metni.
        """
        response = self.claude_completion(
            model=model,
            messages=[{"role": "user", "content": content}]
        )
        return response["content"][0]["text"]


    def __repr__(self) -> str:
        return f"AgentGrid(base_url={self.base_url!r}, api_key={self.api_key!r}, timeout={self.timeout}s)"


# ──────────────────────────────────────────────────────────────────────────────
# Hata Siniflari
# ──────────────────────────────────────────────────────────────────────────────

class AgentGridError(Exception):
    """Sunucu gecersiz HTTP kodu dondurdugunde firlatilir."""

    def __init__(self, status_code: int, message: str):
        self.status_code = status_code
        self.message     = message
        super().__init__(f"HTTP {status_code}: {message}")


class AgentGridConnError(Exception):
    """Sunucuya baglanti kurulamadiginda firlatilir."""
    pass


# ──────────────────────────────────────────────────────────────────────────────
# ORNEK KULLANIM
# ──────────────────────────────────────────────────────────────────────────────
#
# from agent_grid_client import AgentGrid
#
# grid = AgentGrid()
# response = grid.chat_completion(
#     model="gpt-4o",
#     messages=[{"role": "user", "content": "SDK Test Mesaji!"}]
# )
# print("Borsadan Donen Cevap:", response["choices"][0]["message"]["content"])
#
# ---- Diger yontemler ----
#
# # Tek satirda yanitı al:
# yanitı = grid.cheapest_message("gpt-4o", "Borsayı anlat bana.")
# print(yanit)
#
# # Aktif emir defterini goruntule:
# kitap = grid.list_orders()
# print(f"Aktif emir sayisi: {kitap['total_orders']}")
#
# # Elle emir ekle:
# emir = grid.add_order(
#     provider="OpenAI",
#     model="gpt-4o",
#     token_amount=25000,
#     price_per_1k=0.0018
# )
# print(f"Yeni emir olusturuldu: #{emir['id']}")
#
# ──────────────────────────────────────────────────────────────────────────────

if __name__ == "__main__":
    # Dogrudan calistirilirsa hizli baglanti testi yap
    import sys

    if sys.platform == "win32":
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")

    print("Agent Grid SDK -- Baglanti Testi")
    print("-" * 40)

    grid = AgentGrid()
    print(f"Istemci: {grid}")
    print()

    try:
        # Emir defteri
        kitap = grid.list_orders()
        print(f"[OK] Emir defteri erisimi basarili. Aktif emir: {kitap['total_orders']}")

        # Chat completion
        yanit = grid.cheapest_message("gpt-4o", "Merhaba! Bu bir SDK testidir.")
        print(f"[OK] Chat completion basarili.")
        print(f"     Yanit: {yanit[:80]}{'...' if len(yanit) > 80 else ''}")

    except AgentGridConnError as e:
        print(f"[HATA] {e}")
        sys.exit(1)
    except AgentGridError as e:
        print(f"[HATA] Sunucu hatasi {e.status_code}: {e.message}")
        sys.exit(1)
