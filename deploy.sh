#!/bin/bash

# ⚡ Agent Grid Protocol — Ubuntu VPS Auto-Deployment Script
# Bu betik sunucuda root veya sudo yetkileriyle calistirilmalidir.

set -e # Herhangi bir hata durumunda betigi durdur

echo "========================================================"
echo "   AGENT GRID PROTOCOL — VPS DAĞITIM SİSTEMİ"
echo "========================================================"

# 1. Paket Güncellemesi & Gerekli Temel Araçlar
echo "[1/6] Sunucu paket listeleri guncelleniyor..."
sudo apt update && sudo apt upgrade -y
sudo apt install -y curl git build-essential pkg-config libssl-dev

# 2. Rust ve Cargo Kurulumu
if ! command -v cargo &> /dev/null; then
    echo "[2/6] Rust derleyicisi yuklu degil. Kuruluyor..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
else
    echo "[2/6] Rust zaten yuklu."
    source "$HOME/.cargo/env" || true
fi

# 3. Çalışma Dizininin Hazırlanması ve Kodun Çekilmesi
DEPLOY_DIR="/var/www/agent-grid-protocol"
REPO_URL="https://github.com/buraksakmak27-cpu/agent-grid-protocol.git"

echo "[3/6] Kodlar GitHub'dan guncelleniyor..."
if [ -d "$DEPLOY_DIR" ]; then
    echo "Dizin zaten mevcut. Guncel kodlar cekiliyor..."
    cd "$DEPLOY_DIR"
    git fetch --all
    git reset --hard origin/main
else
    echo "Dizin olusturuluyor ve repo clone'laniyor..."
    sudo mkdir -p /var/www
    sudo chown -R $USER:$USER /var/www
    git clone "$REPO_URL" "$DEPLOY_DIR"
    cd "$DEPLOY_DIR"
fi

# 4. Projenin Derlenmesi (Optimized Release Build)
echo "[4/6] Proje en yuksek optimizasyon ile derleniyor (cargo build --release)..."
cargo build --release

# 5. Systemd Servisinin Yapılandırılması
echo "[5/6] Systemd servis dosyasi olusturuluyor..."
SERVICE_FILE="/etc/systemd/system/agent_grid.service"

sudo bash -c "cat > $SERVICE_FILE" <<EOL
[Unit]
Description=Agent Grid Protocol Exchange Server
After=network.target

[Service]
Type=simple
User=$USER
WorkingDirectory=$DEPLOY_DIR
ExecStart=$DEPLOY_DIR/target/release/token_borsasi
Restart=always
RestartSec=5
# İsteğe bağlı ortam değişkenlerinizi buraya ekleyebilirsiniz:
# Environment=OPENAI_API_KEY=your_key
# Environment=ANTHROPIC_API_KEY=your_key

[Install]
WantedBy=multi-user.target
EOL

# 6. Servisin Başlatılması ve Etkinleştirilmesi
echo "[6/6] Servis etkinlestiriliyor ve baslatiliyor..."
sudo systemctl daemon-reload
sudo systemctl enable agent_grid
sudo systemctl restart agent_grid

echo "========================================================"
echo "   KURULUM TAMAMLANDI! 🚀"
echo "========================================================"
echo "Servis Durumu:   sudo systemctl status agent_grid"
echo "Canlı Günlükler: sudo journalctl -u agent_grid -f"
echo "Dashboard:       http://<sunucu_ip_adresi>:3000/dashboard"
echo "========================================================"
