use std::collections::HashMap;

/// Kullanıcının havuzdaki staking pozisyonu
#[derive(Debug, Clone)]
pub struct StakingPosition {
    pub user_api_key: String,
    pub token_amount: f64,
    pub staked_at: u64,
    pub reward_multiplier: f64,
}

/// USDC likidite kilitleyerek pasif gelir kazandıran ve piyasa yapıcı sermayesini fonlayan Staking & Likidite Havuzu
#[derive(Debug, Clone)]
pub struct StakingPool {
    pub positions: HashMap<String, StakingPosition>,
    pub total_staked: f64,
    /// Tüm kullanıcıların stake ettiği ve komisyonların toplandığı küresel likidite havuzu (USDC/USD)
    pub global_liquidity: f64,
}

impl Default for StakingPool {
    fn default() -> Self {
        Self::new()
    }
}

impl StakingPool {
    /// Yeni bir staking havuzu oluşturur
    pub fn new() -> Self {
        Self {
            positions: HashMap::new(),
            total_staked: 0.0,
            global_liquidity: 1000.0, // Başlangıç likiditesi bootstrap sermaye olarak 1000 USD
        }
    }

    /// Havuza USDC staking işlemi yapar (Global Likidite Havuzuna ekler)
    pub fn stake(&mut self, user_api_key: String, amount: f64) -> bool {
        if amount <= 0.0 {
            return false;
        }
        
        let pos = self.positions.entry(user_api_key.clone()).or_insert_with(|| StakingPosition {
            user_api_key,
            token_amount: 0.0,
            staked_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_secs(),
            reward_multiplier: 1.1, // %10 bonus çarpanı
        });

        pos.token_amount += amount;
        self.total_staked += amount;
        self.global_liquidity += amount;
        
        println!(
            "[STAKING] Kullanıcı {} Havuza ${:.4} USDC kilitledi. Toplam Stake: ${:.4} | Küresel Likidite: ${:.4}",
            pos.user_api_key, amount, self.total_staked, self.global_liquidity
        );
        true
    }

    /// Birikmiş staking ödülünü hesaplar
    pub fn calculate_reward(&self, user_api_key: &str) -> f64 {
        if let Some(pos) = self.positions.get(user_api_key) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_secs();
            let duration = now.saturating_sub(pos.staked_at);
            // Zaman bazlı ödül formülü: token * süre * çarpan * katsayı
            pos.token_amount * (duration as f64) * 0.00000005 * pos.reward_multiplier
        } else {
            0.0
        }
    }

    /// Piyasa yapıcı (Market Maker) botu için kullanılabilecek sermaye limiti sorgular
    pub fn get_available_capital(&self) -> f64 {
        self.global_liquidity
    }
}
