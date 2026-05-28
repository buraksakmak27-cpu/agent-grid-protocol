use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use crate::SharedState;

#[derive(Deserialize)]
pub struct LemonSqueezyPayload {
    pub meta: Option<LemonMeta>,
    pub data: LemonData,
}

#[derive(Deserialize)]
pub struct LemonMeta {
    pub custom_data: Option<Value>,
}

#[derive(Deserialize)]
pub struct LemonData {
    pub attributes: LemonAttributes,
}

#[derive(Deserialize)]
pub struct LemonAttributes {
    pub total: f64, // total in cents, e.g. 1000 is $10.00
    pub currency: String,
    pub status: String,
}

/// POST /api/pay/lemonsqueezy-webhook
pub async fn handle_lemonsqueezy_webhook(
    State(state): State<SharedState>,
    Json(payload): Json<LemonSqueezyPayload>,
) -> impl IntoResponse {
    let api_key = payload.meta
        .as_ref()
        .and_then(|m| m.custom_data.as_ref())
        .and_then(|cd| cd.get("api_key"))
        .and_then(|ak| ak.as_str())
        .unwrap_or("anonymous")
        .to_string();

    let amount = payload.data.attributes.total / 100.0; // convert cents to USD

    if amount <= 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Geçersiz miktar" })),
        ).into_response();
    }

    if payload.data.attributes.status != "paid" {
        return (
            StatusCode::OK,
            Json(json!({ "status": "ignored", "message": "Ödeme tamamlanmamış" })),
        ).into_response();
    }

    state.write().unwrap()
        .ledger.credit_usd(&api_key, amount);

    let wallet = state.read().unwrap()
        .ledger.wallets.get(&api_key).cloned()
        .unwrap_or_default();

    println!(
        "[LEMONSQUEEZY] Ödeme alındı | Kullanıcı: {} | +${:.4} USD | Yeni USD bakiye: ${:.4}",
        api_key, amount, wallet.usd_balance
    );

    (StatusCode::OK, Json(json!({
        "status":      "credited",
        "api_key":     api_key,
        "credited":    amount,
        "usd_balance": wallet.usd_balance,
    }))).into_response()
}
