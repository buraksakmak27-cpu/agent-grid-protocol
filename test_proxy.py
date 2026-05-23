# -*- coding: utf-8 -*-
"""
Agent Grid -- Proxy Tunel Testi
Yerel Token Borsasi sunucusuna (127.0.0.1:3000) OpenAI uyumlu istek atar.
"""

import sys
import json
import requests

# Windows terminal encoding sorununu onle
if sys.platform == "win32":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")

BASE_URL = "http://127.0.0.1:3000/v1"

payload = {
    "model": "gpt-4o",
    "messages": [
        {"role": "user", "content": "Bu istek Agent Grid proxy testidir!"}
    ]
}

print("=" * 60)
print("  TOKEN BORSASI -- Proxy Tunel Testi")
print("=" * 60)
print(f"  Hedef : {BASE_URL}/chat/completions")
print(f"  Model : {payload['model']}")
print(f"  Mesaj : {payload['messages'][0]['content']}")
print("=" * 60)
print()

try:
    response = requests.post(
        f"{BASE_URL}/chat/completions",
        headers={"Content-Type": "application/json"},
        json=payload,
        timeout=10
    )

    print(f"[HTTP] Durum Kodu : {response.status_code}")
    print()

    if response.status_code != 200:
        print(f"[HATA] Sunucu hatasi: {response.text}")
        sys.exit(1)

    data = response.json()

    print("[YANIT] " + "-" * 50)
    print(f"  ID             : {data.get('id', 'N/A')}")
    print(f"  Model          : {data.get('model', 'N/A')}")
    print(f"  Fingerprint    : {data.get('system_fingerprint', 'N/A')}")
    print()

    choices = data.get("choices", [])
    if choices:
        msg = choices[0].get("message", {})
        print(f"  Rol            : {msg.get('role', 'N/A')}")
        print(f"  Icerik         : {msg.get('content', 'N/A')}")
        print(f"  Bitis Nedeni   : {choices[0].get('finish_reason', 'N/A')}")

    usage = data.get("usage", {})
    if usage:
        print()
        print(f"  Kullanim       : {usage.get('prompt_tokens', 0)} prompt + "
              f"{usage.get('completion_tokens', 0)} completion = "
              f"{usage.get('total_tokens', 0)} toplam token")

    print()
    print("=" * 60)
    print("  [OK] TEST BASARILI -- Proxy tuneli calisiyor!")
    print("=" * 60)

except requests.exceptions.ConnectionError:
    print("[HATA] Sunucuya baglanılamadı.")
    print("  'cargo run' ile sunucunun calistiginden emin olun.")
    sys.exit(1)
except Exception as e:
    print(f"[HATA] Beklenmeyen hata: {e}")
    sys.exit(1)
