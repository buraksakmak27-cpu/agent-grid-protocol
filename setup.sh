#!/bin/bash

# ⚡ Token Borsası — VPS Auto-Setup & Clean-up Script
# Bu betik sunucudaki disk alanını temizler, projeyi GitHub'dan çeker, derler ve pm2 ile arka planda çalıştırır.

set -e # Herhangi bir hata durumunda betiği durdur

echo "========================================================"
# Logo ve Hoşgeldiniz
echo "   TOKEN BORSASI — VPS SETUP & CLEAN-UP SİSTEMİ"
echo "========================================================"

# 1. Sunucu Disk Alanı Temizliği & Gereksiz Paketlerin Silinmesi
echo "[1/6] Sunucu disk alanı temizleniyor..."
sudo apt-get clean
sudo apt-get autoremove -y
# Journalctl log limitini temizle (disk alanı kazanmak için)
sudo journalctl --vacuum-time=1d || true
# npm önbelleğini temizle
npm cache clean --force || true
echo "✔ Disk temizliği tamamlandı."

# 2. Eski Dizinlerin Temizlenmesi
echo "[2/6] Eski token_borsasi dizinleri temizleniyor..."
# Eski pm2 süreçlerini durdur (varsa port çatışmasını engellemek için)
if command -v pm2 &> /dev/null; then
    echo "Mevcut PM2 süreçleri durduruluyor..."
    pm2 delete token-borsasi-backend || true
    pm2 delete token-borsasi-frontend || true
fi
# Dizinleri temizle
rm -rf ~/token_borsasi
rm -rf /var/www/agent-grid-protocol
echo "✔ Eski dizinler temizlendi."

# 3. Gerekli Bağımlılıkların Kurulum Kontrolü (Node, Rust, Git, vb.)
echo "[3/6] Gerekli sistem araçları kontrol ediliyor..."
sudo apt update
sudo apt install -y curl git build-essential pkg-config libssl-dev

# Node.js & npm Kontrolü/Kurulumu
if ! command -v node &> /dev/null; then
    echo "Node.js kuruluyor..."
    curl -fsSL https://deb.nodesource.com/setup_18.x | sudo -E bash -
    sudo apt-get install -y nodejs
fi

# PM2 Kontrolü/Kurulumu
if ! command -v pm2 &> /dev/null; then
    echo "PM2 global olarak kuruluyor..."
    sudo npm install -g pm2
fi

# Rust Kontrolü/Kurulumu
if ! command -v cargo &> /dev/null; then
    echo "Rust/Cargo kuruluyor..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
else
    source "$HOME/.cargo/env" || true
fi
echo "✔ Sistem araçları hazır."

# 4. Kodların GitHub'dan Çekilmesi
echo "[4/6] Proje GitHub üzerinden clone ediliyor..."
REPO_URL="https://github.com/buraksakmak27-cpu/agent-grid-protocol.git"
git clone "$REPO_URL" ~/token_borsasi
cd ~/token_borsasi

# 5. Projenin Derlenmesi ve İnşa Edilmesi (Frontend & Backend)
echo "[5/6] Proje derleniyor..."

# Rust Backend Derleme
echo "Rust Backend derleniyor (cargo build --release)..."
cargo build --release

# Frontend Dashboard Derleme
echo "Next.js Frontend derleniyor..."
cd dashboard
npm install
npm run build
cd ..

echo "✔ Derleme işlemleri başarıyla tamamlandı."

# 6. PM2 ile Uygulamaların Portları Ayarlanarak Başlatılması
echo "[6/6] Uygulamalar PM2 ile arka planda başlatılıyor..."

# 3001 portunda Rust Backend'i başlat
pm2 start ./target/release/token_borsasi --name "token-borsasi-backend"

# 3000 portunda Next.js Frontend'i başlat
cd dashboard
pm2 start npm --name "token-borsasi-frontend" -- run start -- -p 3000
cd ..

# PM2 Yapılandırmasını Kaydet
pm2 save

echo "========================================================"
echo "🚀 KURULUM VE BAŞLATMA BAŞARIYLA TAMAMLANDI!"
echo "========================================================"
echo "• Rust Backend Portu:  3001"
echo "• Next.js Frontend Portu: 3000"
echo ""
echo "Süreç Kontrolleri İçin Komutlar:"
echo "  - PM2 Süreç Listesi:    pm2 list"
echo "  - Backend Logları:      pm2 logs token-borsasi-backend"
echo "  - Frontend Logları:     pm2 logs token-borsasi-frontend"
echo "  - Sunucu Reboot Sonrası Otomatik Başlatma: pm2 startup"
echo "========================================================"
