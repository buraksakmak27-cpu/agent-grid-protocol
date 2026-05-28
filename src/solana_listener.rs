use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::signature::Signature;
use solana_transaction_status::UiTransactionEncoding;
use std::sync::{Arc, RwLock};
use std::collections::HashSet;
use std::str::FromStr;
use tokio::time::{sleep, Duration};

use crate::SharedState;

/// Solana ağı üzerinde belirtilen borsa cüzdanına gelen USDC SPL token transferlerini dinleyen ve doğrulayan servis
pub struct SolanaListener {
    rpc_client: Arc<RpcClient>,
    target_wallet: String,
    state: SharedState,
    processed_signatures: RwLock<HashSet<String>>,
}

struct ParsedTransfer {
    sender: String,
    amount: f64,
}

impl SolanaListener {
    /// Yeni bir Solana USDC dinleyicisi oluşturur
    pub fn new(rpc_url: &str, target_wallet: &str, state: SharedState) -> Self {
        Self {
            rpc_client: Arc::new(RpcClient::new(rpc_url.to_string())),
            target_wallet: target_wallet.to_string(),
            state,
            processed_signatures: RwLock::new(HashSet::new()),
        }
    }

    /// Solana ağını sürekli tarayan asenkron döngü
    pub async fn start_listening(&self) {
        println!("[SOLANA LISTENER] Solana USDC Transfer Dinleyici Başlatıldı.");
        println!("[SOLANA LISTENER] Dinlenen Hedef Borsa Cüzdanı: {}", self.target_wallet);

        let recipient_pubkey = match solana_sdk::pubkey::Pubkey::from_str(&self.target_wallet) {
            Ok(pk) => pk,
            Err(e) => {
                eprintln!("[SOLANA LISTENER] HATA: Geçersiz hedef cüzdan adresi: {}", e);
                return;
            }
        };

        loop {
            // Her 15 saniyede bir yeni işlemleri tara
            sleep(Duration::from_secs(15)).await;

            let config = solana_client::rpc_client::GetConfirmedSignaturesForAddress2Config {
                before: None,
                until: None,
                limit: Some(10),
                commitment: Some(solana_sdk::commitment_config::CommitmentConfig::confirmed()),
            };

            let signatures = match self.rpc_client.get_signatures_for_address_with_config(&recipient_pubkey, config).await {
                Ok(sigs) => sigs,
                Err(e) => {
                    eprintln!("[SOLANA LISTENER] İmza listesi alınırken hata oluştu: {}", e);
                    continue;
                }
            };

            for sig_info in signatures {
                let sig_str = sig_info.signature;
                
                // Zaten işlendi mi kontrolü
                {
                    let processed = self.processed_signatures.read().unwrap();
                    if processed.contains(&sig_str) {
                        continue;
                    }
                }

                let signature = match Signature::from_str(&sig_str) {
                    Ok(s) => s,
                    Err(_) => continue,
                };

                let tx_config = solana_client::rpc_config::RpcTransactionConfig {
                    encoding: Some(UiTransactionEncoding::JsonParsed),
                    max_supported_transaction_version: Some(0),
                    commitment: Some(solana_sdk::commitment_config::CommitmentConfig::confirmed()),
                };

                let tx = match self.rpc_client.get_transaction_with_config(&signature, tx_config).await {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("[SOLANA LISTENER] İşlem detayları alınamadı ({}): {}", sig_str, e);
                        continue;
                    }
                };

                if let Some(parsed_transfer) = self.parse_usdc_transfer(&tx) {
                    let amount = parsed_transfer.amount;
                    let sender = parsed_transfer.sender;

                    // Alıcıyı veritabanında bulup bakiyesini artır
                    let mut app = self.state.write().unwrap();
                    if let Some(api_key) = app.ledger.wallet_addresses.get(&sender).cloned() {
                        app.ledger.credit_usd(&api_key, amount);
                        println!(
                            "[SOLANA LISTENER] Gerçek USDC Yatırma Tespit Edildi! TX: {} | Gönderen: {} | Alıcı API Anahtarı: {} | Tutar: ${:.4} USDC",
                            sig_str, sender, api_key, amount
                        );
                    } else {
                        println!(
                            "[SOLANA LISTENER] Ödeme tespit edildi ancak gönderici adresi ({}) kayıtlı bir API anahtarıyla eşleşmiyor. (Tutar: ${:.4} USDC)",
                            sender, amount
                        );
                    }
                }

                // İşlendi olarak işaretle
                {
                    let mut processed = self.processed_signatures.write().unwrap();
                    processed.insert(sig_str);
                }
            }
        }
    }

    /// SPL-Token USDC transfer detaylarını parsed JSON üzerinden ayıklar
    fn parse_usdc_transfer(&self, tx: &solana_transaction_status::EncodedConfirmedTransactionWithStatusMeta) -> Option<ParsedTransfer> {
        let meta = tx.transaction.meta.as_ref()?;
        if meta.err.is_some() {
            return None;
        }

        let encoded_tx = &tx.transaction.transaction;
        if let solana_transaction_status::EncodedTransaction::Json(ui_tx) = encoded_tx {
            if let solana_transaction_status::UiMessage::Parsed(parsed_msg) = &ui_tx.message {
                for inst in &parsed_msg.instructions {
                    if let solana_transaction_status::UiInstruction::Parsed(parsed_inst) = inst {
                        if let solana_transaction_status::UiParsedInstruction::Parsed(parsed_data) = parsed_inst {
                            if parsed_data.program == "spl-token" {
                                let info = parsed_data.parsed.get("info")?;
                                let parsed_type = parsed_data.parsed.get("type")?.as_str()?;

                                if parsed_type == "transfer" || parsed_type == "transferChecked" {
                                    let amount_str = info.get("amount")
                                        .and_then(|a| a.as_str())
                                        .or_else(|| info.get("tokenAmount").and_then(|ta| ta.get("amount")).and_then(|a| a.as_str()))?;
                                    
                                    let amount = amount_str.parse::<f64>().ok()? / 1_000_000.0;
                                    let sender = info.get("authority")?.as_str()?.to_string();

                                    return Some(ParsedTransfer { sender, amount });
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }
}
