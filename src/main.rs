use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use ordered_float::OrderedFloat;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, RwLock},
    str::FromStr,
};
use tokio::time::{sleep, Duration};
use tower_http::cors::{Any, CorsLayer};

use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::signature::Signature;
use solana_transaction_status::UiTransactionEncoding;

// ═══════════════════════════════════════════════════════════════════════════
// VERİ YAPILARI
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Provider {
    OpenAI,
    Anthropic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id:           u64,
    pub provider:     Provider,
    pub model:        String,
    pub token_amount: u32,
    pub price_per_1k: f64,
}

// ═══════════════════════════════════════════════════════════════════════════
// CÜZDAN & LEDGER
// ═══════════════════════════════════════════════════════════════════════════

/// İşlem başına düşülen sembolik ücret (USD).
const TX_FEE_USD: f64 = 0.001;

/// Her kullanıcıya ait çift bakiyeli cüzdan.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserWallet {
    /// Stripe / Lemon Squeezy kredi kartı bakiyesi (USD).
    pub usd_balance:    f64,
    /// Solana / USDC kripto bakiyesi (USD).
    pub crypto_balance: f64,
}

impl UserWallet {
    /// Toplam kullanılabilir bakiye.
    pub fn total(&self) -> f64 {
        self.usd_balance + self.crypto_balance
    }

    /// Önce kripto bakiyeden, yetmezse USD bakiyeden düş.
    pub fn deduct(&mut self, amount: f64) -> bool {
        if self.total() < amount {
            return false;
        }
        let from_crypto = amount.min(self.crypto_balance);
        self.crypto_balance -= from_crypto;
        let remaining = amount - from_crypto;
        self.usd_balance -= remaining;
        true
    }
}

/// Tüm kullanıcıların cüzdanlarını tutan ledger.
/// Anahtar: kullanıcının API key'i (veya bot adı).
#[derive(Debug, Default)]
pub struct WalletLedger {
    pub wallets: HashMap<String, UserWallet>,
}

impl WalletLedger {
    /// Kullanıcıyı oluştur veya mevcut cüzdanı getir.
    pub fn get_or_create(&mut self, api_key: &str) -> &mut UserWallet {
        self.wallets.entry(api_key.to_string()).or_default()
    }

    /// USD bakiyesine ekle (Stripe webhook).
    pub fn credit_usd(&mut self, api_key: &str, amount: f64) {
        self.get_or_create(api_key).usd_balance += amount;
    }

    /// Kripto bakiyesine ekle (USDC doğrulama).
    pub fn credit_crypto(&mut self, api_key: &str, amount: f64) {
        self.get_or_create(api_key).crypto_balance += amount;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// EMIR DEFTERİ
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Default)]
pub struct OrderBook {
    pub asks: BTreeMap<OrderedFloat<f64>, Vec<Order>>,
    next_id:  u64,
}

impl OrderBook {
    pub fn insert(
        &mut self,
        provider:     Provider,
        model:        String,
        token_amount: u32,
        price_per_1k: f64,
    ) -> Order {
        self.next_id += 1;
        let order = Order { id: self.next_id, provider, model, token_amount, price_per_1k };
        self.asks.entry(OrderedFloat(price_per_1k)).or_default().push(order.clone());
        order
    }

    pub fn consume(&mut self, model: &str) -> Option<Order> {
        let mut hit_price: Option<OrderedFloat<f64>> = None;
        let mut hit_idx:   Option<usize>             = None;

        'search: for (price, orders) in &self.asks {
            for (i, o) in orders.iter().enumerate() {
                if o.model == model {
                    hit_price = Some(*price);
                    hit_idx   = Some(i);
                    break 'search;
                }
            }
        }

        let (price, idx) = hit_price.zip(hit_idx)?;
        let orders  = self.asks.get_mut(&price)?;
        let matched = orders[idx].clone();

        if matched.token_amount <= 1_000 {
            orders.remove(idx);
            if orders.is_empty() { self.asks.remove(&price); }
        } else {
            orders[idx].token_amount -= 1_000;
        }
        Some(matched)
    }

    pub fn all_orders(&self) -> Vec<Order> {
        self.asks.values().flatten().cloned().collect()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// PAYLAŞIMLI UYGULAMA DURUMU
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Default)]
pub struct AppState {
    pub order_book: OrderBook,
    pub ledger:     WalletLedger,
}

pub type SharedState = Arc<RwLock<AppState>>;

/// Gelen istekteki `X-Api-Key` header'ını çıkarır; yoksa "anonymous" döner.
fn extract_api_key(headers: &HeaderMap) -> String {
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("anonymous")
        .to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// ÖDEME HANDLER'LARI
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct StripeWebhookReq {
    api_key: String,
    amount:  f64,
    /// Opsiyonel: Stripe ödeme niyeti ID'si (simülasyon için görmezden gelinir)
    #[serde(default)]
    payment_intent: String,
}

/// POST /api/pay/stripe-webhook
/// Stripe / Lemon Squeezy başarılı ödeme webhook'unu simüle eder.
async fn handle_stripe_webhook(
    State(state): State<SharedState>,
    Json(req): Json<StripeWebhookReq>,
) -> impl IntoResponse {
    if req.amount <= 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Geçersiz miktar" })),
        ).into_response();
    }

    state.write().unwrap()
        .ledger.credit_usd(&req.api_key, req.amount);

    let wallet = state.read().unwrap()
        .ledger.wallets.get(&req.api_key).cloned()
        .unwrap_or_default();

    println!(
        "[STRIPE] Odeme alindi | Kullanici: {} | +${:.4} USD | Yeni USD bakiye: ${:.4}",
        req.api_key, req.amount, wallet.usd_balance
    );

    (StatusCode::OK, Json(json!({
        "status":      "credited",
        "api_key":     req.api_key,
        "credited":    req.amount,
        "usd_balance": wallet.usd_balance,
        "payment_intent": req.payment_intent,
    }))).into_response()
}

#[derive(Deserialize)]
struct CryptoVerifyReq {
    api_key:  String,
    tx_hash:  String,
    amount:   f64,
    /// Opsiyonel: Solana / EVM chain bilgisi
    #[serde(default = "default_chain")]
    chain:    String,
}
fn default_chain() -> String { "solana".to_string() }

const RECIPIENT_WALLET: &str = "Gr1dLedgerWa11etAddressUSDC1111111111111";

/// POST /api/pay/crypto-verify
/// Kullanıcının USDC gönderi TX'ini doğrular ve kripto bakiyeye ekler.
async fn handle_crypto_verify(
    State(state): State<SharedState>,
    Json(req): Json<CryptoVerifyReq>,
) -> impl IntoResponse {
    if req.amount <= 0.0 || req.tx_hash.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Geçersiz TX hash veya miktar" })),
        ).into_response();
    }

    // Geliştirme/test kolaylığı için mock işlemlere izin verelim
    if req.tx_hash.contains("mock") || req.tx_hash.len() < 30 {
        state.write().unwrap()
            .ledger.credit_crypto(&req.api_key, req.amount);

        let wallet = state.read().unwrap()
            .ledger.wallets.get(&req.api_key).cloned()
            .unwrap_or_default();

        println!(
            "[CRYPTO MOCK] TX dogrulandi | {} | Kullanici: {} | +${:.4} USDC | Kripto bakiye: ${:.4}",
            req.tx_hash, req.api_key, req.amount, wallet.crypto_balance
        );

        return (StatusCode::OK, Json(json!({
            "status":          "verified_and_credited",
            "api_key":         req.api_key,
            "tx_hash":         req.tx_hash,
            "chain":           req.chain,
            "credited_usdc":   req.amount,
            "crypto_balance":  wallet.crypto_balance,
            "verified_onchain": false,
        }))).into_response();
    }

    // Gerçek Solana On-chain doğrulaması
    let signature = match Signature::from_str(&req.tx_hash) {
        Ok(sig) => sig,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Geçersiz Solana Signature formatı" })),
            ).into_response();
        }
    };

    // Solana Mainnet RPC istemcisi
    let rpc_client = RpcClient::new("https://api.mainnet-beta.solana.com".to_string());

    // 1. Signature durumunu sorgula
    match rpc_client.get_signature_status(&signature).await {
        Ok(Some(status_res)) => {
            if let Err(err) = status_res {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("Solana işlemi ağda hata aldı: {:?}", err) })),
                ).into_response();
            }
        }
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "İşlem Solana ağında bulunamadı. Lütfen TX hash'i kontrol edin." })),
            ).into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Solana RPC bağlantı hatası: {}", e) })),
            ).into_response();
        }
    }

    // 2. Transaction detaylarını kontrol et
    let tx_config = solana_client::rpc_config::RpcTransactionConfig {
        encoding: Some(UiTransactionEncoding::JsonParsed),
        max_supported_transaction_version: Some(0),
        ..Default::default()
    };

    let mut onchain_verified = false;

    match rpc_client.get_transaction_with_config(&signature, tx_config).await {
        Ok(tx) => {
            let tx_json = serde_json::to_string(&tx).unwrap_or_default();
            
            // Recipient cüzdan adresi ve USDC Token Mint adresleri (Mainnet & Devnet)
            let recipient_found = tx_json.contains(RECIPIENT_WALLET);
            let usdc_mint_found = tx_json.contains("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v") 
                || tx_json.contains("4zMMC9srt5Ri4HPuRxVxuGVspHwfqj37JGN8u6VJZ43u");

            if !recipient_found {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("Alıcı cüzdan adresi geçerli borsa cüzdanı ({}) değil.", RECIPIENT_WALLET) })),
                ).into_response();
            }

            if !usdc_mint_found {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "İşlem USDC transferi içermiyor." })),
                ).into_response();
            }

            onchain_verified = true;
        }
        Err(e) => {
            // Kamusal RPC node'larında get_transaction sorgusu kısıtlanmış olabilir.
            println!(
                "[CRYPTO UYARI] get_transaction RPC sorgusu kısıtlı veya hata verdi: {}. Sadece signature onay durumuyla devam ediliyor.",
                e
            );
        }
    }

    // Kredilendir
    state.write().unwrap()
        .ledger.credit_crypto(&req.api_key, req.amount);

    let wallet = state.read().unwrap()
        .ledger.wallets.get(&req.api_key).cloned()
        .unwrap_or_default();

    println!(
        "[CRYPTO ON-CHAIN] TX dogrulandi | {} | Kullanici: {} | +${:.4} USDC | Kripto bakiye: ${:.4}",
        req.tx_hash, req.api_key, req.amount, wallet.crypto_balance
    );

    (StatusCode::OK, Json(json!({
        "status":          "verified_and_credited",
        "api_key":         req.api_key,
        "tx_hash":         req.tx_hash,
        "chain":           req.chain,
        "credited_usdc":   req.amount,
        "crypto_balance":  wallet.crypto_balance,
        "verified_onchain": onchain_verified,
    }))).into_response()
}

/// GET /api/wallet — Kullanıcı bakiyesini sorgular (X-Api-Key header)
async fn handle_get_wallet(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let api_key = extract_api_key(&headers);
    let state   = state.read().unwrap();
    let wallet  = state.ledger.wallets.get(&api_key).cloned().unwrap_or_default();

    Json(json!({
        "api_key":         api_key,
        "usd_balance":     wallet.usd_balance,
        "crypto_balance":  wallet.crypto_balance,
        "total_balance":   wallet.total(),
        "tx_fee_per_req":  TX_FEE_USD,
    }))
}

// ═══════════════════════════════════════════════════════════════════════════
// EMIR VE KİTAP HANDLER'LARI
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct AddOrderReq {
    provider:     Provider,
    model:        String,
    token_amount: u32,
    price_per_1k: f64,
}

/// POST /order
async fn handle_add_order(
    State(state): State<SharedState>,
    Json(req): Json<AddOrderReq>,
) -> impl IntoResponse {
    let order = state.write().unwrap()
        .order_book.insert(req.provider, req.model, req.token_amount, req.price_per_1k);
    println!(
        "[EMİR] #{:>4} | {} token | ${:.6}/1k",
        order.id, order.token_amount, order.price_per_1k
    );
    (StatusCode::CREATED, Json(order))
}

/// GET /book
async fn handle_get_book(State(state): State<SharedState>) -> impl IntoResponse {
    let orders = state.read().unwrap().order_book.all_orders();
    Json(json!({ "total_orders": orders.len(), "asks": orders }))
}

// ═══════════════════════════════════════════════════════════════════════════
// BAKİYE KONTROLÜ (ortak yardımcı)
// ═══════════════════════════════════════════════════════════════════════════

/// Kullanıcının bakiyesini kontrol eder ve TX ücretini düşer.
/// Başarısızsa `Some(402 Response)`, başarıysa `None` döner.
fn check_and_charge(state: &SharedState, api_key: &str) -> Option<Response> {
    let mut app = state.write().unwrap();
    let wallet  = app.ledger.get_or_create(api_key);

    if wallet.total() < TX_FEE_USD {
        println!(
            "[BAKIYE YETERSIZ] Kullanici islem yapamadi | api_key: {} | Bakiye: ${:.6}",
            api_key, wallet.total()
        );
        return Some((
            StatusCode::PAYMENT_REQUIRED,
            Json(json!({
                "error": "Payment Required",
                "message": "Yetersiz bakiye. Lutfen /api/pay/stripe-webhook veya /api/pay/crypto-verify ile yukleme yapin.",
                "usd_balance":    wallet.usd_balance,
                "crypto_balance": wallet.crypto_balance,
                "required":       TX_FEE_USD,
            })),
        ).into_response());
    }

    wallet.deduct(TX_FEE_USD);
    println!(
        "[ODEME] ${:.6} tahsil edildi | api_key: {} | Kalan toplam: ${:.6}",
        TX_FEE_USD, api_key, wallet.total()
    );
    None
}

// ═══════════════════════════════════════════════════════════════════════════
// OPENAI PROXY
// ═══════════════════════════════════════════════════════════════════════════

/// POST /v1/chat/completions — OpenAI uyumlu proxy tüneli
async fn handle_proxy(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let api_key = extract_api_key(&headers);

    // Bakiye kontrolü
    if let Some(err) = check_and_charge(&state, &api_key) {
        return err;
    }

    let body_json: Value = match serde_json::from_slice(&body) {
        Ok(v)  => v,
        Err(_) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Gecersiz JSON" })),
        ).into_response(),
    };

    let model = body_json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("gpt-4o")
        .to_string();

    // Emir tüket
    {
        let mut app = state.write().unwrap();
        match app.order_book.consume(&model) {
            Some(o) => println!(
                "[PROXY] Esleme | Emir #{} | {} | Kalan {} token | ${:.6}/1k",
                o.id, o.model, o.token_amount.saturating_sub(1_000), o.price_per_1k
            ),
            None => println!("[PROXY] Uyari: '{}' icin aktif emir bulunamadi.", model),
        }
    }

    let openai_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if !openai_key.is_empty() {
        let client = reqwest::Client::new();
        match client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", openai_key))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
        {
            Ok(resp) => {
                let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
                let json: Value = resp.json().await
                    .unwrap_or_else(|_| json!({ "error": "upstream parse hatasi" }));
                (status, Json(json)).into_response()
            }
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("Upstream hatasi: {}", e) })),
            ).into_response(),
        }
    } else {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_secs();
        let mock = json!({
            "id":     format!("chatcmpl-mock-{:08x}", rand::thread_rng().gen::<u32>()),
            "object": "chat.completion",
            "created": ts,
            "model":  model,
            "choices": [{ "index": 0, "message": {
                "role": "assistant",
                "content": "Merhaba! Agent Grid uzerinden hizmet veren bir yapay zeka ajaniyim. Bu yanit Token Borsasi simulasyonu tarafindan uretilmistir."
            }, "finish_reason": "stop", "logprobs": null }],
            "usage": { "prompt_tokens": 25, "completion_tokens": 35, "total_tokens": 60 },
            "system_fingerprint": "agent-grid-v1"
        });
        println!("[MOCK] {} icin simule yanit uretildi.", model);
        (StatusCode::OK, Json(mock)).into_response()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ANTHROPIC CLAUDE PROXY
// ═══════════════════════════════════════════════════════════════════════════

/// POST /v1/messages — Anthropic Claude SDK'larıyla tam uyumlu proxy tüneli
async fn handle_anthropic_proxy(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let api_key = extract_api_key(&headers);

    // Bakiye kontrolü
    if let Some(err) = check_and_charge(&state, &api_key) {
        return err;
    }

    let body_json: Value = match serde_json::from_slice(&body) {
        Ok(v)  => v,
        Err(_) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "Gecersiz JSON" } })),
        ).into_response(),
    };

    let model = body_json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("claude-3-5-sonnet")
        .to_string();

    {
        let mut app = state.write().unwrap();
        match app.order_book.consume(&model) {
            Some(o) => println!(
                "[CLAUDE PROXY] Istek, ID: {} olan en ucuz Anthropic kotasiyla eslesti! | {} | Kalan {} token | ${:.6}/1k",
                o.id, o.model, o.token_amount.saturating_sub(1_000), o.price_per_1k
            ),
            None => println!(
                "[CLAUDE PROXY] Uyari: '{}' icin aktif Anthropic kotasi bulunamadi.",
                model
            ),
        }
    }

    let anthropic_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    if !anthropic_key.is_empty() {
        let client = reqwest::Client::new();
        match client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &anthropic_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
        {
            Ok(resp) => {
                let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
                let json: Value = resp.json().await
                    .unwrap_or_else(|_| json!({ "type": "error", "error": { "message": "upstream parse hatasi" } }));
                (status, Json(json)).into_response()
            }
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "type": "error", "error": { "message": format!("Upstream hatasi: {}", e) } })),
            ).into_response(),
        }
    } else {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_secs();
        let mock_id = format!("msg_mock_{:016x}", ts);
        let mock = json!({
            "id":      mock_id,
            "type":    "message",
            "role":    "assistant",
            "content": [{ "type": "text", "text":
                "Merhaba! Ben Agent Grid uzerinden yonlendirilen otonom Claude ajaniyim. Bu yanit Token Borsasi simulasyonu tarafindan uretilmistir."
            }],
            "model":         model,
            "stop_reason":   "end_turn",
            "stop_sequence": null,
            "usage": { "input_tokens": 20, "output_tokens": 45 }
        });
        println!("[CLAUDE MOCK] {} icin simule Anthropic yaniti uretildi.", model);
        (StatusCode::OK, Json(mock)).into_response()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// WEB DASHBOARD ARAYÜZÜ
// ═══════════════════════════════════════════════════════════════════════════

/// GET /dashboard — Gömülü web arayüzünü döner
async fn handle_dashboard() -> impl IntoResponse {
    Html(DASHBOARD_HTML.to_string())
}

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="tr">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Agent Grid Protocol - Dashboard</title>
    <script src="https://cdn.tailwindcss.com"></script>
    <link href="https://fonts.googleapis.com/css2?family=Outfit:wght@300;400;600;700&family=JetBrains+Mono:wght@400;700&display=swap" rel="stylesheet">
    <style>
        body {
            font-family: 'Outfit', sans-serif;
            background-color: #05070c;
        }
        .font-mono {
            font-family: 'JetBrains Mono', monospace;
        }
        .neon-glow-cyan {
            text-shadow: 0 0 10px rgba(6, 182, 212, 0.4), 0 0 20px rgba(6, 182, 212, 0.2);
        }
        .neon-border-cyan {
            box-shadow: 0 0 15px rgba(6, 182, 212, 0.15);
        }
        .neon-glow-green {
            text-shadow: 0 0 10px rgba(34, 197, 94, 0.4), 0 0 20px rgba(34, 197, 94, 0.2);
        }
        .cyber-grid {
            background-image: linear-gradient(rgba(6, 182, 212, 0.03) 1px, transparent 1px),
                              linear-gradient(90deg, rgba(6, 182, 212, 0.03) 1px, transparent 1px);
            background-size: 20px 20px;
        }
    </style>
</head>
<body class="text-slate-100 min-h-screen cyber-grid flex flex-col justify-between">
    <!-- Navbar / Header -->
    <header class="border-b border-cyan-500/20 bg-slate-950/80 backdrop-blur-md py-4 px-6 sticky top-0 z-50">
        <div class="max-w-7xl mx-auto flex justify-between items-center">
            <div class="flex items-center gap-3">
                <span class="text-2xl font-bold text-cyan-400 font-mono tracking-wider neon-glow-cyan">⚡ AGENT_GRID</span>
                <span class="text-xs px-2 py-0.5 rounded border border-cyan-500/30 bg-cyan-950/50 text-cyan-300 font-mono">v0.2.0 PROTOCOL</span>
            </div>
            <div class="flex items-center gap-4">
                <div class="text-right hidden sm:block">
                    <p class="text-xs text-slate-400">Node Status</p>
                    <p class="text-xs text-green-400 font-mono flex items-center gap-1.5 justify-end">
                        <span class="w-2 h-2 rounded-full bg-green-500 animate-pulse"></span> ONLINE
                    </p>
                </div>
            </div>
        </div>
    </header>

    <!-- Main Content -->
    <main class="flex-grow max-w-7xl w-full mx-auto p-4 sm:p-6 lg:p-8 grid grid-cols-1 lg:grid-cols-5 gap-6">
        
        <!-- Left Panel: Live Order Book (3 cols) -->
        <section class="lg:col-span-3 flex flex-col gap-6">
            <div class="bg-slate-900/60 backdrop-blur-md rounded-xl border border-cyan-500/20 p-6 flex flex-col h-[600px] neon-border-cyan">
                <div class="flex justify-between items-center mb-4 border-b border-cyan-500/10 pb-3">
                    <h2 class="text-lg font-bold tracking-wider font-mono text-cyan-400 flex items-center gap-2">
                        <span class="w-2 h-2 bg-cyan-500 rounded-full animate-ping"></span>
                        LIVE ORDER BOOK (ASK SIDE)
                    </h2>
                    <span id="order-count" class="text-xs font-mono bg-cyan-950 text-cyan-300 px-2 py-1 rounded border border-cyan-500/20">0 ORDERS ACTIVE</span>
                </div>

                <!-- Table Container -->
                <div class="overflow-y-auto flex-grow pr-1 custom-scrollbar">
                    <table class="w-full text-left font-mono text-sm">
                        <thead>
                            <tr class="text-slate-400 border-b border-slate-800">
                                <th class="py-2.5">ID</th>
                                <th class="py-2.5 font-semibold text-cyan-400">PROVIDER</th>
                                <th class="py-2.5">MODEL</th>
                                <th class="py-2.5 text-right font-semibold text-cyan-400">VOLUME</th>
                                <th class="py-2.5 text-right text-emerald-400">PRICE/1K</th>
                            </tr>
                        </thead>
                        <tbody id="order-book-body">
                            <tr>
                                <td colspan="5" class="py-12 text-center text-slate-500">Piyasa verisi yükleniyor...</td>
                            </tr>
                        </tbody>
                    </table>
                </div>

                <div class="mt-4 pt-3 border-t border-cyan-500/10 flex justify-between text-xs text-slate-400 font-mono">
                    <span>* Matched automatically using cheapest-first priority</span>
                    <span>Auto-refreshes every 2s</span>
                </div>
            </div>
        </section>

        <!-- Right Panel: Wallet & Deposits (2 cols) -->
        <section class="lg:col-span-2 flex flex-col gap-6">
            
            <!-- Wallet Panel -->
            <div class="bg-slate-900/60 backdrop-blur-md rounded-xl border border-cyan-500/20 p-6 neon-border-cyan">
                <h2 class="text-lg font-bold tracking-wider font-mono text-cyan-400 mb-4 border-b border-cyan-500/10 pb-3">
                    MY CREDENTIALS & WALLET
                </h2>

                <!-- API Key Input -->
                <div class="mb-4">
                    <label class="block text-xs font-mono text-slate-400 mb-1.5 uppercase">API Developer Key / ID</label>
                    <div class="flex gap-2">
                        <input id="api-key-input" type="text" value="ali_dev_123" 
                            class="bg-slate-950 border border-cyan-500/30 rounded px-3 py-1.5 text-sm font-mono text-cyan-300 focus:outline-none focus:border-cyan-400 flex-grow">
                        <button id="refresh-wallet-btn" onclick="fetchWallet()" 
                            class="bg-cyan-950 text-cyan-400 border border-cyan-500/30 rounded px-3 text-sm font-semibold hover:bg-cyan-900 transition font-mono">
                            REFRESH
                        </button>
                    </div>
                </div>

                <!-- Balance Grid -->
                <div class="grid grid-cols-2 gap-4">
                    <div class="bg-slate-950/60 border border-slate-800 rounded-lg p-3">
                        <span class="block text-slate-400 text-xs font-mono">STRIPE BALANCE</span>
                        <span id="wallet-usd" class="text-xl font-bold font-mono text-slate-200">$0.0000</span>
                    </div>
                    <div class="bg-slate-950/60 border border-slate-800 rounded-lg p-3">
                        <span class="block text-slate-400 text-xs font-mono">SOLANA USDC</span>
                        <span id="wallet-crypto" class="text-xl font-bold font-mono text-emerald-400 neon-glow-green">$0.0000</span>
                    </div>
                    <div class="col-span-2 bg-cyan-950/30 border border-cyan-500/20 rounded-lg p-4 flex justify-between items-center">
                        <div>
                            <span class="block text-slate-400 text-xs font-mono">TOTAL FUNDS AVAILABLE</span>
                            <span id="wallet-total" class="text-2xl font-bold font-mono text-cyan-300 neon-glow-cyan">$0.0000</span>
                        </div>
                        <div class="text-right">
                            <span class="block text-slate-500 text-[10px] font-mono">PROXY TX FEE</span>
                            <span class="text-xs font-mono text-slate-400">$0.001 / req</span>
                        </div>
                    </div>
                </div>
            </div>

            <!-- Deposit Panel -->
            <div class="bg-slate-900/60 backdrop-blur-md rounded-xl border border-cyan-500/20 p-6 neon-border-cyan flex-grow flex flex-col justify-between">
                <div>
                    <h2 class="text-lg font-bold tracking-wider font-mono text-cyan-400 mb-4 border-b border-cyan-500/10 pb-3">
                        DEPOSIT PORTAL (MOCK ACCREDITATION)
                    </h2>

                    <!-- Stripe payment mock -->
                    <div class="mb-6">
                        <div class="flex justify-between items-center mb-2">
                            <h3 class="text-sm font-semibold font-mono text-slate-300">OPTION A: STRIPE / CARD</h3>
                            <span class="text-[10px] text-cyan-400 bg-cyan-950 px-1.5 py-0.5 rounded border border-cyan-500/20 font-mono">INSTANT</span>
                        </div>
                        <p class="text-xs text-slate-400 mb-3">Kredi kartı ödemesini simüle ederek bakiyenize anında USD yükleyin.</p>
                        <div class="flex gap-2">
                            <span class="bg-slate-950 border border-slate-800 rounded px-2.5 py-1.5 text-sm font-mono text-slate-400 flex items-center">$</span>
                            <input id="stripe-amount-input" type="number" value="10.00" step="1" min="1"
                                class="bg-slate-950 border border-slate-800 rounded px-3 py-1.5 text-sm font-mono text-slate-200 focus:outline-none focus:border-cyan-500 w-24">
                            <button onclick="depositStripe()" 
                                class="bg-cyan-500 hover:bg-cyan-400 text-slate-950 font-bold px-4 py-1.5 rounded transition text-sm flex-grow font-mono">
                                LEMON SQUEEZY YÜKLE
                            </button>
                        </div>
                    </div>

                    <!-- Crypto payment mock -->
                    <div>
                        <div class="flex justify-between items-center mb-2">
                            <h3 class="text-sm font-semibold font-mono text-slate-300">OPTION B: SOLANA USDC</h3>
                            <span class="text-[10px] text-emerald-400 bg-emerald-950 px-1.5 py-0.5 rounded border border-emerald-500/20 font-mono">ON-CHAIN</span>
                        </div>
                        <p class="text-xs text-slate-400 mb-2">Cüzdanımıza USDC transferini simüle edin. TX kodunu ve miktarı yazın:</p>
                        <div class="flex flex-col gap-2">
                            <div class="flex gap-2">
                                <span class="bg-slate-950 border border-slate-800 rounded px-2.5 py-1.5 text-xs font-mono text-slate-400 flex items-center">USDC</span>
                                <input id="crypto-amount-input" type="number" value="25.00" step="1" min="1"
                                    class="bg-slate-950 border border-slate-800 rounded px-3 py-1.5 text-sm font-mono text-slate-200 focus:outline-none focus:border-cyan-500 w-24">
                                <input id="crypto-tx-input" type="text" placeholder="Solana Tx Hash" 
                                    class="bg-slate-950 border border-slate-800 rounded px-3 py-1.5 text-sm font-mono text-slate-200 focus:outline-none focus:border-cyan-500 flex-grow">
                            </div>
                            <button onclick="depositCrypto()" 
                                class="bg-emerald-500 hover:bg-emerald-400 text-slate-950 font-bold px-4 py-2 rounded transition text-sm font-mono">
                                SOLANA İLE USDC GÖNDER
                            </button>
                        </div>
                    </div>
                </div>

                <!-- Toast Notifications -->
                <div id="toast" class="mt-4 p-2 bg-cyan-950/60 border border-cyan-500/30 text-cyan-300 text-xs font-mono rounded hidden flex justify-between items-center">
                    <span id="toast-message">Bakiye başarıyla güncellendi!</span>
                    <button onclick="this.parentElement.classList.add('hidden')" class="hover:text-cyan-100 font-bold ml-2">×</button>
                </div>
            </div>
        </section>

    </main>

    <!-- Footer -->
    <footer class="border-t border-cyan-500/10 py-4 px-6 text-center text-xs text-slate-500 font-mono">
        &copy; 2026 Agent Grid Protocol. Autonomous AI Token & API Quota Exchange. Built with Axum & Rust.
    </footer>

    <!-- JavaScript logic -->
    <script>
        // Sayfa yüklenince verileri çek
        window.addEventListener('load', () => {
            // Rastgele bir Solana TX hash'i üretip inputa koyalım
            const randHex = Array.from({length: 44}, () => Math.floor(Math.random()*16).toString(16)).join('');
            document.getElementById('crypto-tx-input').value = randHex.slice(0, 16) + '...sol_mock';
            
            fetchBook();
            fetchWallet();
            
            // Canlı borsa için her 2 saniyede bir fetchBook
            setInterval(fetchBook, 2000);
        });

        // Toast gösterimi
        function showToast(msg, isSuccess = true) {
            const toast = document.getElementById('toast');
            const toastMsg = document.getElementById('toast-message');
            toastMsg.innerText = msg;
            toast.className = `mt-4 p-2.5 text-xs font-mono rounded flex justify-between items-center ` + 
                (isSuccess 
                    ? `bg-emerald-950/60 border border-emerald-500/40 text-emerald-300` 
                    : `bg-red-950/60 border border-red-500/40 text-red-300`);
            toast.classList.remove('hidden');
            setTimeout(() => {
                toast.classList.add('hidden');
            }, 5000);
        }

        // GET /book verilerini çek ve tabloyu güncelle
        async function fetchBook() {
            try {
                const response = await fetch('/book');
                if (!response.ok) return;
                const data = await response.json();
                
                // Sipariş listesini doldur
                const tbody = document.getElementById('order-book-body');
                const orderCount = document.getElementById('order-count');
                
                orderCount.innerText = `${data.total_orders} ORDERS ACTIVE`;
                
                if (!data.asks || data.asks.length === 0) {
                    tbody.innerHTML = `<tr><td colspan="5" class="py-12 text-center text-slate-500">Aktif emir bulunmamaktadır. Botların girmesi bekleniyor...</td></tr>`;
                    return;
                }

                // Fiyata göre sıralı asks
                tbody.innerHTML = data.asks.map(o => {
                    const provClass = o.provider === 'OpenAI' ? 'text-sky-400' : 'text-amber-400';
                    return `
                        <tr class="border-b border-slate-800 hover:bg-slate-900/30 transition">
                            <td class="py-2.5 font-mono text-slate-500">#${o.id}</td>
                            <td class="py-2.5 font-bold ${provClass}">${o.provider}</td>
                            <td class="py-2.5 font-mono text-slate-300">${o.model}</td>
                            <td class="py-2.5 text-right font-mono text-slate-300">${o.token_amount.toLocaleString()}</td>
                            <td class="py-2.5 text-right font-mono font-semibold text-emerald-400">$${o.price_per_1k.toFixed(6)}</td>
                        </tr>
                    `;
                }).join('');

            } catch (err) {
                console.error("Fetch book error:", err);
            }
        }

        // GET /api/wallet bakiye sorgusu
        async function fetchWallet() {
            const apiKey = document.getElementById('api-key-input').value.trim() || 'anonymous';
            try {
                const response = await fetch('/api/wallet', {
                    headers: {
                        'x-api-key': apiKey
                    }
                });
                if (!response.ok) return;
                const data = await response.json();
                
                document.getElementById('wallet-usd').innerText = `$${data.usd_balance.toFixed(4)}`;
                document.getElementById('wallet-crypto').innerText = `$${data.crypto_balance.toFixed(4)}`;
                document.getElementById('wallet-total').innerText = `$${data.total_balance.toFixed(4)}`;
            } catch (err) {
                console.error("Fetch wallet error:", err);
            }
        }

        // POST /api/pay/stripe-webhook ile ödeme yükle
        async function depositStripe() {
            const apiKey = document.getElementById('api-key-input').value.trim() || 'anonymous';
            const amount = parseFloat(document.getElementById('stripe-amount-input').value);
            
            if (isNaN(amount) || amount <= 0) {
                showToast("Lütfen geçerli bir yükleme miktarı girin.", false);
                return;
            }

            try {
                const response = await fetch('/api/pay/stripe-webhook', {
                    method: 'POST',
                    headers: {
                        'Content-Type': 'application/json'
                    },
                    body: JSON.stringify({
                        api_key: apiKey,
                        amount: amount,
                        payment_intent: 'pi_web_dash_' + Math.random().toString(36).substring(7)
                    })
                });

                if (response.ok) {
                    const data = await response.json();
                    showToast(`Başarılı! Kredi Kartı ile $${amount.toFixed(2)} USD hesabınıza aktarıldı.`, true);
                    fetchWallet();
                } else {
                    showToast("Stripe webhook yükleme hatası oluştu.", false);
                }
            } catch (err) {
                showToast("Yükleme işlemi başarısız.", false);
            }
        }

        // POST /api/pay/crypto-verify ile ödeme yükle
        async function depositCrypto() {
            const apiKey = document.getElementById('api-key-input').value.trim() || 'anonymous';
            const amount = parseFloat(document.getElementById('crypto-amount-input').value);
            const txHash = document.getElementById('crypto-tx-input').value.trim();
            
            if (isNaN(amount) || amount <= 0 || !txHash) {
                showToast("Lütfen geçerli miktar ve Solana TX Hash girin.", false);
                return;
            }

            try {
                const response = await fetch('/api/pay/crypto-verify', {
                    method: 'POST',
                    headers: {
                        'Content-Type': 'application/json'
                    },
                    body: JSON.stringify({
                        api_key: apiKey,
                        tx_hash: txHash,
                        amount: amount,
                        chain: 'solana'
                    })
                });

                if (response.ok) {
                    showToast(`Başarılı! Solana USDC $${amount.toFixed(2)} bakiyeniz doğrulandı.`, true);
                    fetchWallet();
                    
                    // Sonraki işlem için yeni bir hash üretelim
                    const randHex = Array.from({length: 44}, () => Math.floor(Math.random()*16).toString(16)).join('');
                    document.getElementById('crypto-tx-input').value = randHex.slice(0, 16) + '...sol_mock';
                } else {
                    showToast("USDC doğrulama hatası oluştu.", false);
                }
            } catch (err) {
                showToast("Doğrulama işlemi başarısız.", false);
            }
        }
    </script>
</body>
</html>"##;

// ═══════════════════════════════════════════════════════════════════════════
// PİYASA YAPICI SIMÜLATÖR
// ═══════════════════════════════════════════════════════════════════════════

fn start_market_making(state: SharedState) {
    tokio::spawn(async move {
        println!("[BOT] Piyasa yapici simulasyon baslatildi — emirler uretiliyor...\n");

        loop {
            let (delay, use_openai, price, tokens) = {
                let mut rng    = rand::thread_rng();
                let delay      = rng.gen_range(2u64..=4u64);
                let use_openai = rng.gen_bool(0.5);
                let (p_min, p_max) = if use_openai { (0.0010f64, 0.0025f64) } else { (0.0020f64, 0.0045f64) };
                let price          = rng.gen_range(p_min..p_max);
                let tokens: u32    = rng.gen_range(5u32..=50u32) * 1_000;
                (delay, use_openai, price, tokens)
            };

            sleep(Duration::from_secs(delay)).await;

            let (provider, model) = if use_openai {
                (Provider::OpenAI,    "gpt-4o")
            } else {
                (Provider::Anthropic, "claude-3-5-sonnet")
            };

            let order = state.write().unwrap()
                .order_book.insert(provider, model.to_string(), tokens, price);

            println!(
                "[BOT HACMI] Yeni Kota Eklendi | #{:>4} | {:<10} {:<22} | {:>6} token | ${:.6}/1k",
                order.id,
                if use_openai { "OpenAI" } else { "Anthropic" },
                order.model,
                order.token_amount,
                order.price_per_1k,
            );
        }
    });
}

// ═══════════════════════════════════════════════════════════════════════════
// ANA FONKSİYON
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() {
    println!("╔══════════════════════════════════════════════════════╗");
    println!("║                                                      ║");
    println!("║        TOKEN BORSASI — Agent Grid  v0.2.0            ║");
    println!("║        Yapay Zeka API Kota Borsasi + Fintech         ║");
    println!("║                                                      ║");
    println!("╚══════════════════════════════════════════════════════╝\n");

    let state: SharedState = Arc::new(RwLock::new(AppState::default()));

    start_market_making(state.clone());

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        // Dashboard
        .route("/dashboard",                    get(handle_dashboard))
        // Borsa
        .route("/order",                        post(handle_add_order))
        .route("/book",                         get(handle_get_book))
        // Proxy tünelleri (bakiye korumalı)
        .route("/v1/chat/completions",          post(handle_proxy))
        .route("/v1/messages",                  post(handle_anthropic_proxy))
        // Fintech ödeme altyapısı
        .route("/api/pay/stripe-webhook",       post(handle_stripe_webhook))
        .route("/api/pay/crypto-verify",        post(handle_crypto_verify))
        .route("/api/wallet",                   get(handle_get_wallet))
        .layer(cors)
        .with_state(state);

    let addr = "127.0.0.1:3000";
    println!("[SISTEM] Sunucu baslatiliyor → http://{}\n", addr);
    println!("  Dashboard:");
    println!("    GET   /dashboard");
    println!("  Borsa:");
    println!("    POST  /order");
    println!("    GET   /book");
    println!("  Proxy (bakiye korumalı):");
    println!("    POST  /v1/chat/completions   (OpenAI)");
    println!("    POST  /v1/messages           (Anthropic Claude)");
    println!("  Fintech:");
    println!("    POST  /api/pay/stripe-webhook");
    println!("    POST  /api/pay/crypto-verify");
    println!("    GET   /api/wallet\n");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
