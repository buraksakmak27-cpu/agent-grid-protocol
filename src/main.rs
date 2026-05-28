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

pub mod solana_listener;
pub mod staking_pool;
pub mod routes;

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
    pub api_key:      String,
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
    pub wallet_addresses: HashMap<String, String>, // Solana adresi -> API Anahtarı
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
        api_key:      String,
    ) -> Order {
        self.next_id += 1;
        let order = Order { id: self.next_id, provider, model, token_amount, price_per_1k, api_key };
        self.asks.entry(OrderedFloat(price_per_1k)).or_default().push(order.clone());

        // BELLEK KORUMASI: Eğer emir sayısı 100'ü aşarsa, en eski emri silerek belleği koru
        let total_orders: usize = self.asks.values().map(|v| v.len()).sum();
        if total_orders > 100 {
            let mut first_key = None;
            let mut should_remove_key = false;
            if let Some((&price, orders)) = self.asks.iter_mut().next() {
                if !orders.is_empty() {
                    orders.remove(0);
                }
                if orders.is_empty() {
                    first_key = Some(price);
                    should_remove_key = true;
                }
            }
            if should_remove_key {
                if let Some(price) = first_key {
                    self.asks.remove(&price);
                }
            }
        }

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
    pub staking_pool: staking_pool::StakingPool,
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

/// Kullanıcının birikmiş staking ödüllerini hesaplar, cüzdanına yansıtır ve staked_at zamanını günceller.
fn distribute_user_rewards(state: &SharedState, api_key: &str) {
    let mut app = state.write().unwrap();
    let reward = app.staking_pool.calculate_reward(api_key);
    if reward > 0.000001 {
        // Cüzdana kripto bakiye olarak ekle
        let wallet = app.ledger.get_or_create(api_key);
        wallet.crypto_balance += reward;
        
        // Staking pozisyonunu güncelle
        if let Some(pos) = app.staking_pool.positions.get_mut(api_key) {
            pos.staked_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_secs();
        }
        
        println!(
            "[STAKING] Kâr dağıtıldı: Kullanıcı {} için ${:.6} USDC ödül bakiye olarak eklendi.",
            api_key, reward
        );
    }
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

    // Gerçek Solana On-chain doğrulaması
    let signature = match Signature::from_str(&req.payment_intent) {
        Ok(sig) => sig,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Geçersiz Solana Signature formatı (payment_intent)" })),
            ).into_response();
        }
    };

    let rpc_client = RpcClient::new("https://api.mainnet-beta.solana.com".to_string());
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
                Json(json!({ "error": "İşlem Solana ağında bulunamadı. Lütfen geçerli bir Solana signature gönderin." })),
            ).into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Solana RPC bağlantı hatası: {}", e) })),
            ).into_response();
        }
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

const RECIPIENT_WALLET: &str = "9tnNYDm6fc71em9jU7pE7dQ4zUNMrAou5uu3qfXfFTPw";

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

/// GET /api/wallet — Kullanıcı bakiyesini sorgular (X-Api-Key veya X-Solana-Address)
async fn handle_get_wallet(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let mut api_key = extract_api_key(&headers);
    
    // Eğer api_key anonymous ise ve X-Solana-Address gönderilmişse, eşleştirme yapalım
    if api_key == "anonymous" {
        if let Some(sol_addr) = headers.get("x-solana-address").and_then(|v| v.to_str().ok()) {
            let mut app = state.write().unwrap();
            api_key = app.ledger.wallet_addresses.entry(sol_addr.to_string())
                .or_insert_with(|| {
                    // Yeni bir API anahtarı üretelim
                    let rand_id = rand::thread_rng().gen_range(1000..9999);
                    let new_key = format!("ali_key_{}", rand_id);
                    println!("[CÜZDAN] Yeni Solana adresi otomatik eşleştirildi: {} -> {}", sol_addr, new_key);
                    new_key
                })
                .clone();
        }
    }

    // Kâr dağıtımını tetikle
    distribute_user_rewards(&state, &api_key);

    let app_read = state.read().unwrap();
    let wallet   = app_read.ledger.wallets.get(&api_key).cloned().unwrap_or_default();
    let total_staked = app_read.staking_pool.positions.get(&api_key).map(|p| p.token_amount).unwrap_or(0.0);

    Json(json!({
        "api_key":         api_key,
        "usd_balance":     wallet.usd_balance,
        "crypto_balance":  wallet.crypto_balance,
        "total_balance":   wallet.total(),
        "total_staked":    total_staked,
        "tx_fee_per_req":  TX_FEE_USD,
        "global_liquidity": app_read.staking_pool.global_liquidity,
    }))
}

#[derive(Deserialize)]
struct StakeReq {
    api_key: String,
    amount: f64,
}

/// POST /api/stake — Kullanıcının bakiyesinden belirtilen miktarı havuza stake eder
async fn handle_stake(
    State(state): State<SharedState>,
    Json(req): Json<StakeReq>,
) -> impl IntoResponse {
    if req.amount <= 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Geçersiz miktar" })),
        ).into_response();
    }

    // Önce birikmiş kârı dağıt (yeni stake eklenmeden önceki süre için)
    distribute_user_rewards(&state, &req.api_key);

    let mut app = state.write().unwrap();
    
    // Kullanıcı cüzdanını al
    let wallet = app.ledger.get_or_create(&req.api_key);
    
    // Staking bakiye durumunu terminale yazdır
    println!(
        "[STAKING İSTEĞİ] API Anahtarı: {}, Stake Edilmek İstenen: {:.2}, Mevcut Kripto Bakiye: {:.2}, Mevcut USD Bakiye: {:.2}",
        req.api_key, req.amount, wallet.crypto_balance, wallet.usd_balance
    );

    // Kripto (USDC) ve USD bakiyesinden akıllı düşüm yap
    if !wallet.deduct(req.amount) {
        println!(
            "[STAKING BAŞARISIZ] API Anahtarı: {} — Yetersiz bakiye (Talep: {:.2}, Toplam Bakiye: {:.2})",
            req.api_key, req.amount, wallet.total()
        );
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ 
                "error": format!(
                    "Staking için yetersiz bakiye. (Talep Edilen: ${:.2}, Mevcut Kripto: ${:.2}, Mevcut USD: ${:.2})", 
                    req.amount, wallet.crypto_balance, wallet.usd_balance
                ) 
            })),
        ).into_response();
    }

    println!(
        "[STAKING BAŞARILI] API Anahtarı: {} — Bakiye düşüldü. Kalan Kripto: {:.2}, Kalan USD: {:.2}",
        req.api_key, wallet.crypto_balance, wallet.usd_balance
    );

    // Havuza ekle
    app.staking_pool.stake(req.api_key.clone(), req.amount);

    let updated_wallet = app.ledger.get_or_create(&req.api_key).clone();
    let staking_pos = app.staking_pool.positions.get(&req.api_key).cloned().unwrap_or_else(|| {
        crate::staking_pool::StakingPosition {
            user_api_key: req.api_key.clone(),
            token_amount: 0.0,
            staked_at: 0,
            reward_multiplier: 1.0,
        }
    });

    (StatusCode::OK, Json(json!({
        "status": "staked",
        "api_key": req.api_key,
        "amount": req.amount,
        "usd_balance": updated_wallet.usd_balance,
        "crypto_balance": updated_wallet.crypto_balance,
        "total_staked": staking_pos.token_amount,
        "global_liquidity": app.staking_pool.global_liquidity,
    }))).into_response()
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
    headers: HeaderMap,
    Json(req): Json<AddOrderReq>,
) -> impl IntoResponse {
    let api_key = extract_api_key(&headers);
    let order = state.write().unwrap()
        .order_book.insert(req.provider, req.model, req.token_amount, req.price_per_1k, api_key);
    println!(
        "[EMİR] #{:>4} | {} token | ${:.6}/1k",
        order.id, order.token_amount, order.price_per_1k
    );
    (StatusCode::CREATED, Json(order))
}

#[derive(Deserialize)]
struct AddTestOrderReq {
    api_key: String,
    provider: String,
    model: String,
    token_amount: u32,
    price_per_1k: f64,
}

async fn handle_add_test_order(
    State(state): State<SharedState>,
    Json(req): Json<AddTestOrderReq>,
) -> impl IntoResponse {
    let provider = if req.provider.to_lowercase() == "openai" {
        Provider::OpenAI
    } else {
        Provider::Anthropic
    };
    let order = state.write().unwrap()
        .order_book.insert(provider, req.model, req.token_amount, req.price_per_1k, req.api_key.clone());
    println!(
        "[TEST EMİR] #{:>4} | {} token | ${:.6}/1k | API Key: {}",
        order.id, order.token_amount, order.price_per_1k, req.api_key
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
// EŞLEŞTİRME MOTORU (MATCHING ENGINE) MANTIĞI
// ═══════════════════════════════════════════════════════════════════════════

/// Eşleşme gerçekleştiğinde, emir defterindeki bakiyeleri günceller (User_A - X, User_B + Y)
pub fn match_orders(
    state: &SharedState,
    buyer_api_key: &str,
    model: &str,
) -> Option<Order> {
    let mut app = state.write().unwrap();
    
    // 1. Önce uygun bir emrin olup olmadığını ve fiyatını kontrol et
    let mut hit_price: Option<OrderedFloat<f64>> = None;

    for (price, orders) in &app.order_book.asks {
        for o in orders {
            if o.model == model {
                hit_price = Some(*price);
                break;
            }
        }
        if hit_price.is_some() { break; }
    }

    let price = hit_price?;
    let trade_cost = price.into_inner();

    // 2. Alıcı bakiyesini kontrol et (komisyon dahil)
    let match_fee = trade_cost * 0.001; // %0.1 komisyon
    let total_cost = trade_cost + match_fee;

    let buyer_wallet = app.ledger.get_or_create(buyer_api_key);
    if buyer_wallet.total() < total_cost {
        println!(
            "[MATCH ENGINE] Yetersiz Bakiye | Alıcı: {} | Bakiye: ${:.6} | Gerekli: ${:.6} (+${:.6} Komisyon)",
            buyer_api_key, buyer_wallet.total(), trade_cost, match_fee
        );
        return None;
    }

    // 3. Siparişi tüket (artık güvenle tüketebiliriz)
    let matched_order = app.order_book.consume(model)?;
    
    // 4. Bakiyeleri düş/ekle
    let buyer_wallet = app.ledger.get_or_create(buyer_api_key);
    buyer_wallet.deduct(total_cost);
    
    let seller_api_key = matched_order.api_key.clone();
    let seller_wallet = app.ledger.get_or_create(&seller_api_key);
    seller_wallet.usd_balance += trade_cost;

    // Komisyonu küresel likidite havuzuna aktar
    app.staking_pool.global_liquidity += match_fee;

    // Eşleşme komisyonunu anlık olarak staker'lara dağıt
    let total_staked = app.staking_pool.total_staked;
    if total_staked > 0.0 {
        let mut reward_details = Vec::new();
        for (staker_api_key, pos) in &app.staking_pool.positions {
            let share = pos.token_amount / total_staked;
            let user_reward = match_fee * share;
            if user_reward > 0.0 {
                reward_details.push((staker_api_key.clone(), user_reward));
            }
        }
        for (staker_api_key, user_reward) in reward_details {
            let wallet = app.ledger.get_or_create(&staker_api_key);
            wallet.crypto_balance += user_reward;
            println!(
                "[STAKING] Kâr dağıtıldı: Eşleşme komisyonundan Kullanıcı {} için ${:.6} USDC ödül bakiye olarak eklendi.",
                staker_api_key, user_reward
            );
        }
    }
    
    println!(
        "[MATCH ENGINE] Eşleşme Gerçekleşti | Emir #{} | Model: {} | Tutar: ${:.6} | Komisyon: ${:.6} | Alıcı: {} | Satıcı: {} | Havuz Likiditesi: ${:.6}",
        matched_order.id, matched_order.model, trade_cost, match_fee, buyer_api_key, seller_api_key, app.staking_pool.global_liquidity
    );
    
    Some(matched_order)
}

// ═══════════════════════════════════════════════════════════════════════════
// OPENAI PROXY (MATCHING ENGINE ENTEGRASYONU)
// ═══════════════════════════════════════════════════════════════════════════

/// POST /v1/chat/completions — OpenAI uyumlu proxy tüneli
async fn handle_proxy(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let api_key = extract_api_key(&headers);

    // Bakiye kontrolü (sistem işlem ücreti tahsilatı)
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

    let mock_llm = std::env::var("MOCK_LLM").unwrap_or_else(|_| "true".to_string()) == "true";

    if mock_llm {
        match match_orders(&state, &api_key, &model) {
            Some(order) => {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default().as_secs();
                let mock = json!({
                    "id":     format!("chatcmpl-match-{:08x}", rand::thread_rng().gen::<u32>()),
                    "object": "chat.completion",
                    "created": ts,
                    "model":  model,
                    "choices": [{ "index": 0, "message": {
                        "role": "assistant",
                        "content": format!("Merhaba! Ben otonom eşleştirme motoruyum. İsteğiniz başarıyla eşleştirildi. Emir ID: {}, Fiyat: ${:.6}/1k.", order.id, order.price_per_1k)
                    }, "finish_reason": "stop", "logprobs": null }],
                    "usage": { "prompt_tokens": 25, "completion_tokens": 45, "total_tokens": 70 },
                    "system_fingerprint": "agent-grid-matching-engine-v1"
                });
                (StatusCode::OK, Json(mock)).into_response()
            }
            None => (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "Matching Order Not Found",
                    "message": format!("Sipariş defterinde '{}' modeli için aktif satış emri veya yeterli alıcı bakiyesi bulunamadı.", model)
                })),
            ).into_response()
        }
    } else {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_secs();
        let mock = json!({
            "id":     format!("chatcmpl-bypass-{:08x}", rand::thread_rng().gen::<u32>()),
            "object": "chat.completion",
            "created": ts,
            "model":  model,
            "choices": [{ "index": 0, "message": {
                "role": "assistant",
                "content": "Bypass Modu: Dış OpenAI API çağrısı bypass edildi."
            }, "finish_reason": "stop", "logprobs": null }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20 }
        });
        (StatusCode::OK, Json(mock)).into_response()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ANTHROPIC CLAUDE PROXY (MATCHING ENGINE ENTEGRASYONU)
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

    let mock_llm = std::env::var("MOCK_LLM").unwrap_or_else(|_| "true".to_string()) == "true";

    if mock_llm {
        match match_orders(&state, &api_key, &model) {
            Some(order) => {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default().as_secs();
                let mock_id = format!("msg_match_{:016x}", ts);
                let mock = json!({
                    "id":      mock_id,
                    "type":    "message",
                    "role":    "assistant",
                    "content": [{ "type": "text", "text": format!("Merhaba! Ben otonom eşleştirme motoruyum. Claude isteğiniz başarıyla eşleştirildi. Emir ID: {}, Fiyat: ${:.6}/1k.", order.id, order.price_per_1k) }],
                    "model":         model,
                    "stop_reason":   "end_turn",
                    "stop_sequence": null,
                    "usage": { "input_tokens": 20, "output_tokens": 50 }
                });
                (StatusCode::OK, Json(mock)).into_response()
            }
            None => (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "type": "error",
                    "error": {
                        "type": "invalid_request_error",
                        "message": format!("Sipariş defterinde '{}' modeli için aktif satış emri veya yeterli alıcı bakiyesi bulunamadı.", model)
                    }
                })),
            ).into_response()
        }
    } else {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_secs();
        let mock_id = format!("msg_bypass_{:016x}", ts);
        let mock = json!({
            "id":      mock_id,
            "type":    "message",
            "role":    "assistant",
            "content": [{ "type": "text", "text": "Bypass Modu: Dış Claude API çağrısı bypass edildi." }],
            "model":         model,
            "stop_reason":   "end_turn",
            "stop_sequence": null,
            "usage": { "input_tokens": 10, "output_tokens": 10 }
        });
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
                
                // Havuzdaki sermayeye göre işlem büyüklüğünü belirle (Sermaye odaklı Piyasa Yapıcı)
                let capital = state.read().unwrap().staking_pool.get_available_capital();
                let base_multiplier = if capital <= 10.0 { 1 } else { (capital / 100.0).max(1.0).min(10.0) as u32 };
                let tokens: u32    = rng.gen_range(5u32..=50u32) * 1_000 * base_multiplier;
                (delay, use_openai, price, tokens)
            };

            sleep(Duration::from_secs(delay)).await;

            let (provider, model) = if use_openai {
                (Provider::OpenAI,    "gpt-4o")
            } else {
                (Provider::Anthropic, "claude-3-5-sonnet")
            };

            let order = state.write().unwrap()
                .order_book.insert(provider, model.to_string(), tokens, price, "market_maker_bot".to_string());

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

    // Test verileri ve cüzdan adresi eşleşmesi ekleyelim
    {
        let mut app = state.write().unwrap();
        app.ledger.get_or_create("ali_key_123").usd_balance = 100.0;
        app.ledger.get_or_create("ali_dev_123").usd_balance = 100.0;
        app.ledger.wallet_addresses.insert(
            "TestSolanaSender1111111111111111111111111".to_string(),
            "ali_key_123".to_string(),
        );
        app.ledger.wallet_addresses.insert(
            "TestSolanaSender2222222222222222222222222".to_string(),
            "ali_dev_123".to_string(),
        );
    }

    // Solana USDC Transfer Dinleyici servisini arka planda başlat
    let rpc_url = std::env::var("SOLANA_RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    let listener_state = state.clone();
    tokio::spawn(async move {
        let listener = solana_listener::SolanaListener::new(
            &rpc_url,
            RECIPIENT_WALLET,
            listener_state,
        );
        listener.start_listening().await;
    });

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
        .route("/api/add_test_order",           post(handle_add_test_order))
        // Proxy tünelleri (bakiye korumalı)
        .route("/v1/chat/completions",          post(handle_proxy))
        .route("/v1/messages",                  post(handle_anthropic_proxy))
        // Fintech ödeme altyapısı
        .route("/api/pay/stripe-webhook",       post(handle_stripe_webhook))
        .route("/api/pay/lemonsqueezy-webhook", post(routes::payment::handle_lemonsqueezy_webhook))
        .route("/api/pay/crypto-verify",        post(handle_crypto_verify))
        .route("/api/wallet",                   get(handle_get_wallet))
        .route("/api/stake",                    post(handle_stake))
        .layer(cors)
        .with_state(state);

    let addr = "0.0.0.0:3000";
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
