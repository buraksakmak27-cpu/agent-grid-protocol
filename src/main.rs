use axum::{
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use ordered_float::OrderedFloat;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};
use tokio::time::{sleep, Duration};
use tower_http::cors::{Any, CorsLayer};

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
// EMIR DEFTERİ
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Default)]
pub struct OrderBook {
    /// Satılık emirler fiyata göre sıralı (en ucuz önce)
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

    /// Verilen model için en ucuz emirden 1000 token tüket; emir biterse sil.
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

pub type SharedState = Arc<RwLock<OrderBook>>;

// ═══════════════════════════════════════════════════════════════════════════
// HTTP HANDLER'LARI
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
        .insert(req.provider, req.model, req.token_amount, req.price_per_1k);
    println!(
        "[EMİR] #{:>4} | {} token | ${:.6}/1k",
        order.id, order.token_amount, order.price_per_1k
    );
    (StatusCode::CREATED, Json(order))
}

/// GET /book
async fn handle_get_book(State(state): State<SharedState>) -> impl IntoResponse {
    let orders = state.read().unwrap().all_orders();
    Json(json!({ "total_orders": orders.len(), "asks": orders }))
}

/// POST /v1/chat/completions  — OpenAI uyumlu proxy tüneli
async fn handle_proxy(State(state): State<SharedState>, body: Bytes) -> Response {
    let body_json: Value = match serde_json::from_slice(&body) {
        Ok(v)  => v,
        Err(_) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Geçersiz JSON" })),
        ).into_response(),
    };

    let model = body_json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("gpt-4o")
        .to_string();

    // Emir tüket
    {
        let mut book = state.write().unwrap();
        match book.consume(&model) {
            Some(o) => println!(
                "[PROXY] Eşleşme | Emir #{} | {} | Kalan {} token | ${:.6}/1k",
                o.id, o.model, o.token_amount.saturating_sub(1_000), o.price_per_1k
            ),
            None => println!("[PROXY] Uyarı: '{}' için aktif emir bulunamadı.", model),
        }
    }

    // Gerçek ve dolu bir API anahtarı varsa ilet, yoksa mock
    let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if !api_key.is_empty() {
        let client = reqwest::Client::new();
        match client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
        {
            Ok(resp) => {
                let status = StatusCode::from_u16(resp.status().as_u16())
                    .unwrap_or(StatusCode::OK);
                let json: Value = resp.json().await
                    .unwrap_or_else(|_| json!({ "error": "upstream parse hatası" }));
                (status, Json(json)).into_response()
            }
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("Upstream hatası: {}", e) })),
            ).into_response(),
        }
    } else {
        // Mock yanıt
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mock = json!({
            "id": format!("chatcmpl-mock-{:08x}", rand::thread_rng().gen::<u32>()),
            "object": "chat.completion",
            "created": ts,
            "model": model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Merhaba! Agent Grid üzerinden hizmet veren bir yapay zeka ajanıyım. \
                                Bu yanıt Token Borsası simülasyonu tarafından üretilmiştir."
                },
                "finish_reason": "stop",
                "logprobs": null
            }],
            "usage": { "prompt_tokens": 25, "completion_tokens": 35, "total_tokens": 60 },
            "system_fingerprint": "agent-grid-v1"
        });
        println!("[MOCK] {} için simüle yanıt üretildi.", model);
        (StatusCode::OK, Json(mock)).into_response()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ANTHROPIC CLAUDE PROXY
// ═══════════════════════════════════════════════════════════════════════════

/// POST /v1/messages — Anthropic Claude SDK'larıyla tam uyumlu proxy tüneli
async fn handle_anthropic_proxy(State(state): State<SharedState>, body: Bytes) -> Response {
    // 1. Gelen JSON'dan model adını oku
    let body_json: Value = match serde_json::from_slice(&body) {
        Ok(v)  => v,
        Err(_) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "Geçersiz JSON" } })),
        ).into_response(),
    };

    let model = body_json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("claude-3-5-sonnet")
        .to_string();

    // 2. Emir defterinden en ucuz Anthropic kotasını tüket
    {
        let mut book = state.write().unwrap();
        match book.consume(&model) {
            Some(o) => println!(
                "[CLAUDE PROXY] Istek, ID: {} olan en ucuz Anthropic kotasiyla eslesti! | {} | Kalan {} token | ${:.6}/1k",
                o.id, o.model, o.token_amount.saturating_sub(1_000), o.price_per_1k
            ),
            None => println!(
                "[CLAUDE PROXY] Uyarı: '{}' için aktif Anthropic kotası bulunamadı.",
                model
            ),
        }
    }

    // 3. Gerçek ve dolu bir API anahtarı varsa Anthropic'e ilet, yoksa mock
    let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    if !api_key.is_empty() {
        let client = reqwest::Client::new();
        match client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
        {
            Ok(resp) => {
                let status = StatusCode::from_u16(resp.status().as_u16())
                    .unwrap_or(StatusCode::OK);
                let json: Value = resp.json().await
                    .unwrap_or_else(|_| json!({ "type": "error", "error": { "message": "upstream parse hatası" } }));
                (status, Json(json)).into_response()
            }
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "type": "error", "error": { "message": format!("Upstream hatası: {}", e) } })),
            ).into_response(),
        }
    } else {
        // Resmi Anthropic Messages API formatında mock yanıt
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mock_id = format!("msg_mock_{:016x}", ts);

        let mock = json!({
            "id":      mock_id,
            "type":    "message",
            "role":    "assistant",
            "content": [{
                "type": "text",
                "text": "Merhaba! Ben Agent Grid üzerinden yönlendirilen otonom Claude ajanıyım. \
                          Bu yanıt Token Borsası simülasyonu tarafından üretilmiştir. \
                          Gerçek bir ANTHROPIC_API_KEY tanımlandığında doğrudan Anthropic API'sine yönlendirilir."
            }],
            "model":         model,
            "stop_reason":   "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens":  20,
                "output_tokens": 45
            }
        });

        println!("[CLAUDE MOCK] {} icin simule Anthropic yaniti uretildi.", model);
        (StatusCode::OK, Json(mock)).into_response()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// PİYASA YAPICI SIMÜLATÖR
// ═══════════════════════════════════════════════════════════════════════════

fn start_market_making(state: SharedState) {
    tokio::spawn(async move {
        println!("[BOT] Piyasa yapıcı başlatıldı — emirler üretiliyor...\n");

        loop {
            // Rastgele değerleri await'ten ÖNCE üret ve drop et (ThreadRng Send değil)
            let (delay, use_openai, price, tokens) = {
                let mut rng = rand::thread_rng();
                let delay      = rng.gen_range(2u64..=4u64);
                let use_openai = rng.gen_bool(0.5);
                let (p_min, p_max) = if use_openai { (0.0010f64, 0.0025f64) } else { (0.0020f64, 0.0045f64) };
                let price          = rng.gen_range(p_min..p_max);
                let tokens: u32    = rng.gen_range(5u32..=50u32) * 1_000;
                (delay, use_openai, price, tokens)
            }; // rng burada drop oluyor → await'e taşınmıyor

            sleep(Duration::from_secs(delay)).await;

            let (provider, model) = if use_openai {
                (Provider::OpenAI,    "gpt-4o")
            } else {
                (Provider::Anthropic, "claude-3-5-sonnet")
            };

            let order = state.write().unwrap()
                .insert(provider, model.to_string(), tokens, price);

            println!(
                "[BOT HACMİ] Yeni Kota Eklendi | #{:>4} | {:<10} {:<22} | {:>6} token | ${:.6}/1k",
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
    println!("║        TOKEN BORSASI — Agent Grid  v0.1.0            ║");
    println!("║        Yapay Zeka API Kota Borsası                   ║");
    println!("║                                                      ║");
    println!("╚══════════════════════════════════════════════════════╝\n");

    let state: SharedState = Arc::new(RwLock::new(OrderBook::default()));

    start_market_making(state.clone());

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/order",                post(handle_add_order))
        .route("/book",                 get(handle_get_book))
        .route("/v1/chat/completions",  post(handle_proxy))
        .route("/v1/messages",          post(handle_anthropic_proxy))
        .layer(cors)
        .with_state(state);

    let addr = "127.0.0.1:3000";
    println!("[SİSTEM] Sunucu başlatılıyor → http://{}\n", addr);
    println!("         POST  /order");
    println!("         GET   /book");
    println!("         POST  /v1/chat/completions   (OpenAI)");
    println!("         POST  /v1/messages           (Anthropic Claude)\n");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
