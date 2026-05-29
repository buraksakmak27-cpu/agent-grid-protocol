"use client";

import React, { useState, useEffect } from "react";
import { Buffer } from "buffer";
import { useWallet } from "@solana/wallet-adapter-react";
import { WalletMultiButton } from "@solana/wallet-adapter-react-ui";
import { PublicKey, Transaction, TransactionInstruction } from "@solana/web3.js";
import { 
  Wallet, 
  Coins, 
  TrendingUp, 
  Layers, 
  CreditCard, 
  Zap, 
  ArrowRight,
  Database,
  Info
} from "lucide-react";

const BACKEND_URL = "139.59.134.47:3001";

const USDC_MINT = new PublicKey("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
const RECIPIENT_WALLET = new PublicKey("9tnNYDm6fc71em9jU7pE7dQ4zUNMrAou5uu3qfXfFTPw");

const getAssociatedTokenAddress = (mint: PublicKey, owner: PublicKey): PublicKey => {
  return PublicKey.findProgramAddressSync(
    [
      owner.toBuffer(),
      new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").toBuffer(),
      mint.toBuffer(),
    ],
    new PublicKey("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")
  )[0];
};

const encodeTransferData = (amount: number): Uint8Array => {
  const arr = new Uint8Array(9);
  arr[0] = 3; // Transfer instruction index
  let amountUnits = Math.round(amount * 1_000_000);
  for (let i = 0; i < 8; i++) {
    arr[1 + i] = amountUnits & 0xff;
    amountUnits = Math.floor(amountUnits / 256);
  }
  return arr;
};

interface Order {
  id: number;
  provider: "OpenAI" | "Anthropic" | string;
  model: string;
  token_amount: number;
  price_per_1k: number;
  api_key: string;
  side: string;
}

interface WalletData {
  api_key: string;
  usd_balance: number;
  crypto_balance: number;
  total_balance: number;
  total_staked: number;
  tx_fee_per_req: number;
  global_liquidity: number;
  apy?: number;
  my_orders?: Order[];
}

export default function Home() {
  const { publicKey, connected, signTransaction } = useWallet();
  const [wallet, setWallet] = useState<WalletData | null>(null);
  const [loading, setLoading] = useState(false);
  const [mounted, setMounted] = useState(false);
  const [connectionSlow, setConnectionSlow] = useState(false);
  const [orders, setOrders] = useState<Order[]>([]);
  const [stakeAmount, setStakeAmount] = useState<string>("");
  const [stripeAmount, setStripeAmount] = useState<string>("50");
  const [cryptoTx, setCryptoTx] = useState<string>("");
  const [cryptoAmount, setCryptoAmount] = useState<string>("100");

  const [stripeLoading, setStripeLoading] = useState(false);
  const [cryptoLoading, setCryptoLoading] = useState(false);
  
  const [stakeStatus, setStakeStatus] = useState<{ type: "success" | "error" | ""; msg: string }>({ type: "", msg: "" });
  const [depositStatus, setDepositStatus] = useState<{ type: "success" | "error" | ""; msg: string }>({ type: "", msg: "" });

  // Order Placement & Matching states
  const [orderSide, setOrderSide] = useState<"Buy" | "Sell">("Sell");
  const [orderProvider, setOrderProvider] = useState<string>("OpenAI");
  const [orderModel, setOrderModel] = useState<string>("gpt-4o");
  const [orderTokens, setOrderTokens] = useState<string>("10000");
  const [orderPrice, setOrderPrice] = useState<string>("0.0015");
  const [orderStatus, setOrderStatus] = useState<{ type: "success" | "error" | ""; msg: string }>({ type: "", msg: "" });
  const [matchLoadingId, setMatchLoadingId] = useState<number | null>(null);

  // Fetch Wallet Data
  const fetchWallet = async () => {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), 3000);
    try {
      const headers: Record<string, string> = {
        "Content-Type": "application/json",
        "Cache-Control": "no-cache",
        "Pragma": "no-cache",
        "Access-Control-Allow-Origin": "*",
      };
      
      // Cüzdan bağlıysa adres bilgisini göndererek API anahtarını otomatik eşleştir
      if (publicKey) {
        headers["x-solana-address"] = publicKey.toBase58();
      } else {
        headers["x-api-key"] = "ali_key_123"; // fallback dev key
      }

      console.log(`[fetchWallet] API İsteği Gönderiliyor: URL = ${BACKEND_URL}/api/wallet`, { headers });

      const res = await fetch(`${BACKEND_URL}/api/wallet?t=${Date.now()}`, { 
        mode: "cors",
        headers,
        cache: "no-store",
        signal: controller.signal
      });
      clearTimeout(timeoutId);
      console.log(`[fetchWallet] API Yanıtı Alındı: Status = ${res.status}`);

      if (res.ok) {
        const data = await res.json();
        console.log(`[fetchWallet] Veri başarıyla çözümlendi:`, data);
        setWallet(data);
        if (data && data.total_staked !== undefined) {
          setLoading(false);
          setConnectionSlow(false);
        }
      } else {
        const errorText = await res.text();
        console.error(`[fetchWallet] API Hatası: Status = ${res.status}, Gövde = ${errorText}`);
      }
    } catch (err: any) {
      clearTimeout(timeoutId);
      console.log("[DEBUG] Bağlantı beklemede...");
    }
  };

  // Fetch Order Book
  const fetchOrderBook = async () => {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), 3000);
    try {
      console.log(`[fetchOrderBook] API İsteği Gönderiliyor: URL = ${BACKEND_URL}/book`);
      const res = await fetch(`${BACKEND_URL}/book?t=${Date.now()}`, {
        mode: "cors",
        cache: "no-store",
        signal: controller.signal,
        headers: {
          "Cache-Control": "no-cache",
          "Pragma": "no-cache",
          "Access-Control-Allow-Origin": "*",
        }
      });
      clearTimeout(timeoutId);
      if (res.ok) {
        const data = await res.json();
        console.log("[fetchOrderBook] Veri başarıyla çözümlendi:", data);
        setOrders(data.asks || []);
      }
    } catch (err: any) {
      clearTimeout(timeoutId);
      console.log("[DEBUG] Bağlantı beklemede...");
    }
  };

  useEffect(() => {
    setMounted(true);
  }, []);

  // 5 saniye boyunca veri gelmezse loading ekranından zorla çıkar ve uyarı göster
  useEffect(() => {
    if (publicKey && loading) {
      const timer = setTimeout(() => {
        setConnectionSlow(true);
        setLoading(false);
      }, 5000);
      return () => clearTimeout(timer);
    }
  }, [publicKey, loading]);

  // Poll Wallet and Order Book
  useEffect(() => {
    fetchWallet();
    fetchOrderBook();

    const interval = setInterval(() => {
      fetchOrderBook();
      fetchWallet();
    }, 2000);

    return () => clearInterval(interval);
  }, [publicKey]);

  // Handle Staking
  const handleStake = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!wallet || !stakeAmount || parseFloat(stakeAmount) <= 0) {
      setStakeStatus({ type: "error", msg: "Lütfen geçerli bir miktar girin." });
      return;
    }

    try {
      const res = await fetch(`${BACKEND_URL}/api/stake`, {
        method: "POST",
        mode: "cors",
        headers: { 
          "Content-Type": "application/json",
          "Access-Control-Allow-Origin": "*"
        },
        body: JSON.stringify({
          api_key: wallet.api_key,
          amount: parseFloat(stakeAmount),
        }),
      });

      const data = await res.json();
      if (res.ok) {
        setStakeStatus({ type: "success", msg: `Başarıyla $${stakeAmount} USDC stake edildi!` });
        setStakeAmount("");
        fetchWallet();
      } else {
        setStakeStatus({ type: "error", msg: data.error || "Staking işlemi başarısız." });
      }
    } catch (err) {
      setStakeStatus({ type: "error", msg: "Sunucu bağlantı hatası." });
    }
  };

  // Handle Mock Stripe Deposit
  const handleStripeDeposit = async () => {
    console.log('Buton tetiklendi');
    if (!wallet || !stripeAmount || parseFloat(stripeAmount) <= 0) return;
    setStripeLoading(true);
    setDepositStatus({ type: "", msg: "" });
    try {
      const res = await fetch(`${BACKEND_URL}/api/pay/stripe-webhook`, {
        method: "POST",
        mode: "cors",
        headers: { 
          "Content-Type": "application/json",
          "Access-Control-Allow-Origin": "*"
        },
        body: JSON.stringify({
          api_key: wallet.api_key,
          amount: parseFloat(stripeAmount),
          payment_intent: "stripe_mock_" + Math.random().toString(36).substring(7),
        }),
      });
      if (res.ok) {
        setDepositStatus({ type: "success", msg: "Başarılı" });
        fetchWallet();
      } else {
        setDepositStatus({ type: "error", msg: "İşlem Başarısız" });
      }
    } catch (err) {
      setDepositStatus({ type: "error", msg: "İşlem Başarısız" });
    } finally {
      setStripeLoading(false);
    }
  };

  // Handle Real USDC Deposit with Phantom Wallet
  const handleCryptoVerify = async () => {
    console.log('Buton tetiklendi');
    if (!wallet || !cryptoAmount || parseFloat(cryptoAmount) <= 0) return;
    if (!publicKey || !signTransaction) {
      setDepositStatus({ type: "error", msg: "Lütfen Phantom cüzdanınızı bağlayın." });
      return;
    }

    setCryptoLoading(true);
    setDepositStatus({ type: "", msg: "" });

    try {
      const amount = parseFloat(cryptoAmount);
      const sourceAta = getAssociatedTokenAddress(USDC_MINT, publicKey);
      const destAta = getAssociatedTokenAddress(USDC_MINT, RECIPIENT_WALLET);

      const instruction = new TransactionInstruction({
        keys: [
          { pubkey: sourceAta, isWritable: true, isSigner: false },
          { pubkey: destAta, isWritable: true, isSigner: false },
          { pubkey: publicKey, isWritable: false, isSigner: true },
        ],
        programId: new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"),
        data: Buffer.from(encodeTransferData(amount)),
      });

      const transaction = new Transaction();
      transaction.add(instruction);

      // Son blockhash'i backend üzerinden sorgula
      const blockhashRes = await fetch(`${BACKEND_URL}/api/solana/latest-blockhash`, {
        mode: "cors",
        headers: { 
          "Access-Control-Allow-Origin": "*"
        }
      });
      if (!blockhashRes.ok) {
        throw new Error("Sunucudan blockhash alınamadı.");
      }
      const { blockhash } = await blockhashRes.json();

      transaction.recentBlockhash = blockhash;
      transaction.feePayer = publicKey;

      // Cüzdan imzalama penceresini tetikle
      const signedTx = await signTransaction(transaction);

      // Serialize et ve base64 formatına çevir
      const rawTx = signedTx.serialize();
      const serializedTxBase64 = Buffer.from(rawTx).toString("base64");

      console.log("[CRYPTO FRONTEND] İşlem Phantom ile imzalandı. Sunucuya gönderiliyor...");

      // Backend API'ye serialized transaction'ı gönder
      const res = await fetch(`${BACKEND_URL}/api/process_transaction`, {
        method: "POST",
        mode: "cors",
        headers: { 
          "Content-Type": "application/json",
          "Access-Control-Allow-Origin": "*"
        },
        body: JSON.stringify({
          api_key: wallet.api_key,
          serialized_tx: serializedTxBase64,
          amount: amount,
        }),
      });

      if (res.ok) {
        setDepositStatus({ type: "success", msg: "Başarılı" });
        setCryptoTx("");
        fetchWallet();
      } else {
        setDepositStatus({ type: "error", msg: "İşlem Başarısız" });
      }
    } catch (err) {
      console.error("[CRYPTO ERROR]", err);
      setDepositStatus({ type: "error", msg: "İşlem Başarısız" });
    } finally {
      setCryptoLoading(false);
    }
  };

  // Handle Place Order
  const handlePlaceOrder = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!wallet) return;

    setOrderStatus({ type: "", msg: "" });

    try {
      const headers: Record<string, string> = {
        "Content-Type": "application/json",
        "Access-Control-Allow-Origin": "*",
      };
      if (publicKey) {
        headers["x-solana-address"] = publicKey.toBase58();
      } else {
        headers["x-api-key"] = wallet.api_key;
      }

      const res = await fetch(`${BACKEND_URL}/api/order`, {
        method: "POST",
        mode: "cors",
        headers,
        body: JSON.stringify({
          side: orderSide,
          provider: orderProvider,
          model: orderModel,
          token_amount: parseInt(orderTokens),
          price_per_1k: parseFloat(orderPrice),
        }),
      });

      if (res.ok) {
        setOrderStatus({ type: "success", msg: "Emir başarıyla girildi." });
        fetchWallet();
        fetchOrderBook();
      } else {
        const errText = await res.text();
        setOrderStatus({ type: "error", msg: `Hata: ${errText || "Emir girilemedi."}` });
      }
    } catch (err) {
      console.error(err);
      setOrderStatus({ type: "error", msg: "Bağlantı hatası." });
    }
  };

  // Handle Match Order
  const handleMatchOrder = async (orderId: number) => {
    if (!wallet) return;
    setMatchLoadingId(orderId);
    try {
      const headers: Record<string, string> = {
        "Content-Type": "application/json",
        "Access-Control-Allow-Origin": "*",
      };
      if (publicKey) {
        headers["x-solana-address"] = publicKey.toBase58();
      } else {
        headers["x-api-key"] = wallet.api_key;
      }

      const res = await fetch(`${BACKEND_URL}/api/match_order`, {
        method: "POST",
        mode: "cors",
        headers,
        body: JSON.stringify({
          order_id: orderId,
        }),
      });

      if (res.ok) {
        alert("Eşleşme Başarılı!");
        fetchWallet();
        fetchOrderBook();
      } else {
        const errData = await res.json();
        alert(`Hata: ${errData.error || "Eşleşme başarısız."}`);
      }
    } catch (err) {
      console.error(err);
      alert("Eşleşme sırasında bir hata oluştu.");
    } finally {
      setMatchLoadingId(null);
    }
  };

  // Hydration Guard - SSR uyumluluğu için mount edilene kadar loading spinner göster
  if (!mounted) {
    return (
      <div className="min-h-screen bg-zinc-950 flex flex-col items-center justify-center text-emerald-400 font-mono">
        <div className="flex flex-col items-center gap-4">
          <div className="h-10 w-10 border-4 border-emerald-500/25 border-t-emerald-400 rounded-full animate-spin"></div>
          <p className="text-sm tracking-widest animate-pulse">Yükleniyor...</p>
        </div>
      </div>
    );
  }



  return (
    <div className="min-h-screen bg-zinc-950 font-sans text-gray-100 flex flex-col antialiased">
      {/* Header */}
      <header className="border-b border-emerald-500/20 bg-zinc-900/40 backdrop-blur sticky top-0 z-50">
        <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8 h-20 flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className="bg-emerald-500/10 p-2.5 rounded-xl border border-emerald-500/30">
              <Zap className="h-6 w-6 text-emerald-400 animate-pulse" />
            </div>
            <div>
              <h1 className="text-xl font-bold tracking-tight text-white flex items-center gap-2">
                AGENT GRID <span className="text-emerald-400 text-xs font-semibold px-2 py-0.5 rounded border border-emerald-400/20 bg-emerald-400/10">CORE</span>
              </h1>
              <p className="text-xs text-zinc-400">Yapay Zeka API Kota & Likidite Borsası</p>
            </div>
          </div>
          <div className="flex items-center gap-4">
            <WalletMultiButton className="!bg-emerald-500 hover:!bg-emerald-600 !transition-colors !rounded-xl !h-11 !font-semibold !text-sm" />
          </div>
        </div>
      </header>

      {/* Main Content */}
      <main className="flex-1 max-w-7xl w-full mx-auto px-4 sm:px-6 lg:px-8 py-8 flex flex-col gap-8">
        
        {connectionSlow && (
          <div className="rounded-2xl border border-yellow-500/20 bg-yellow-500/5 p-5 text-sm text-yellow-400 flex items-center gap-3 animate-pulse">
            <Info className="h-5 w-5 shrink-0 text-yellow-400" />
            <span className="font-semibold">Bağlantı başarılı, ancak şu an emir defteri boş, birazdan güncellenecek.</span>
          </div>
        )}

        {/* Banner / Info */}
        <div className="rounded-2xl border border-emerald-500/10 bg-gradient-to-r from-emerald-500/5 to-zinc-900 p-6 flex flex-col md:flex-row items-center justify-between gap-6">
          <div className="flex items-center gap-4">
            <div className="p-3 bg-emerald-400/10 rounded-xl border border-emerald-400/20 text-emerald-400 shrink-0">
              <Info className="h-6 w-6" />
            </div>
            <div>
              <h2 className="text-lg font-semibold text-white">Zincir Üstü Eşleştirme Aktif</h2>
              <p className="text-sm text-zinc-400 max-w-2xl mt-0.5">
                Solana cüzdanınızı bağlayarak anında otomatik hesap eşleşmesi yapabilir, LemonSqueezy / Crypto ödemeleriyle bakiye yükleyip API kotalarını otonom ticaret motorunda stake edebilirsiniz.
              </p>
            </div>
          </div>
        </div>

        {/* Top Stats */}
        <div className="grid grid-cols-1 md:grid-cols-4 gap-6">
          <div className="bg-zinc-900/50 border border-zinc-800 rounded-2xl p-6 flex items-center justify-between">
            <div>
              <p className="text-xs font-medium text-zinc-400 uppercase tracking-wider">USD Bakiye</p>
              <h3 className="text-2xl font-bold text-white mt-1">
                ${wallet?.usd_balance?.toFixed(2) || "0.00"}
              </h3>
            </div>
            <div className="p-3 bg-blue-500/10 rounded-xl text-blue-400">
              <CreditCard className="h-6 w-6" />
            </div>
          </div>

          <div className="bg-zinc-900/50 border border-zinc-800 rounded-2xl p-6 flex items-center justify-between">
            <div>
              <p className="text-xs font-medium text-zinc-400 uppercase tracking-wider">Kripto Bakiye</p>
              <h3 className="text-2xl font-bold text-white mt-1">
                ${wallet?.crypto_balance?.toFixed(2) || "0.00"} <span className="text-xs text-emerald-400">USDC</span>
              </h3>
            </div>
            <div className="p-3 bg-emerald-500/10 rounded-xl text-emerald-400">
              <Coins className="h-6 w-6" />
            </div>
          </div>

          <div className="bg-zinc-900/50 border border-zinc-800 rounded-2xl p-6 flex items-center justify-between">
            <div>
              <p className="text-xs font-medium text-zinc-400 uppercase tracking-wider">Stake Durumum</p>
              <h3 className="text-2xl font-bold text-emerald-400 mt-1">
                ${wallet?.total_staked?.toFixed(2) || "0.00"}
              </h3>
            </div>
            <div className="p-3 bg-emerald-500/10 rounded-xl text-emerald-400">
              <Layers className="h-6 w-6" />
            </div>
          </div>

          <div className="bg-zinc-900/50 border border-zinc-800 rounded-2xl p-6 flex items-center justify-between">
            <div>
              <p className="text-xs font-medium text-zinc-400 uppercase tracking-wider">Küresel Likidite</p>
              <h3 className="text-2xl font-bold text-white mt-1">
                ${wallet?.global_liquidity?.toFixed(2) || "0.00"}
              </h3>
            </div>
            <div className="p-3 bg-yellow-500/10 rounded-xl text-yellow-400">
              <TrendingUp className="h-6 w-6" />
            </div>
          </div>
        </div>

        {/* Main Grid */}
        <div className="grid grid-cols-1 lg:grid-cols-3 gap-8">
          
          {/* Left Column: Account Details & Staking */}
          <div className="flex flex-col gap-8 lg:col-span-1">
            
            {/* Account Card */}
            <div className="bg-zinc-900/50 border border-zinc-800 rounded-3xl p-6 flex flex-col gap-4">
              <h3 className="text-base font-semibold text-white flex items-center gap-2">
                <Wallet className="h-5 w-5 text-emerald-400" /> Hesap Detayları
              </h3>
              <div className="border-t border-zinc-800/80 my-1"></div>
              <div className="flex flex-col gap-3 text-sm">
                <div className="flex justify-between">
                  <span className="text-zinc-400">API Anahtarı</span>
                  <span className="font-mono text-emerald-400">{wallet?.api_key || "Yükleniyor..."}</span>
                </div>
                <div className="flex justify-between">
                  <span className="text-zinc-400">Solana Cüzdanı</span>
                  <span className="text-zinc-300 font-mono text-xs max-w-[180px] truncate">
                    {publicKey ? publicKey.toBase58() : "Bağlı Değil"}
                  </span>
                </div>
                <div className="flex justify-between">
                  <span className="text-zinc-400">İşlem Komisyonu</span>
                  <span className="text-zinc-300">%0.1</span>
                </div>
              </div>
            </div>

            {/* Staking Card */}
            <div className="bg-zinc-900/50 border border-emerald-500/15 rounded-3xl p-6 flex flex-col gap-4 relative overflow-hidden">
              <div className="absolute top-0 right-0 w-32 h-32 bg-emerald-500/5 rounded-full blur-3xl pointer-events-none"></div>
              <h3 className="text-base font-semibold text-white flex items-center justify-between gap-2">
                <span className="flex items-center gap-2">
                  <Layers className="h-5 w-5 text-emerald-400" /> USDC Likidite Staking
                </span>
                <span className="text-xs font-semibold px-2 py-0.5 rounded border border-emerald-400/20 bg-emerald-400/10 text-emerald-400">
                  %{wallet?.apy?.toFixed(2) || "12.50"} APY
                </span>
              </h3>
              <p className="text-xs text-zinc-400">
                Havuzda stake ettiğiniz bakiyeler piyasa yapıcı botu için sermaye oluşturur. Gerçekleşen her trading işlem komisyonunun %0.1'i havuza aktarılarak getirinizi artırır.
              </p>
              
              <form onSubmit={handleStake} className="flex flex-col gap-3 mt-2">
                <div className="relative">
                  <input
                    type="number"
                    step="0.01"
                    placeholder="Miktar ($ USDC)"
                    value={stakeAmount}
                    onChange={(e) => setStakeAmount(e.target.value)}
                    className="w-full bg-zinc-950/80 border border-zinc-800 focus:border-emerald-500/50 rounded-xl px-4 py-3 text-sm outline-none transition-colors pr-12 text-white"
                  />
                  <span className="absolute right-4 top-3 text-xs font-semibold text-zinc-400">USDC</span>
                </div>
                <button
                  type="submit"
                  className="w-full bg-emerald-500 hover:bg-emerald-600 transition-colors text-zinc-950 font-bold py-3 rounded-xl text-sm flex items-center justify-center gap-2 shadow-lg shadow-emerald-500/10"
                >
                  USDC Stake Et <ArrowRight className="h-4 w-4" />
                </button>
              </form>

              {stakeStatus.msg && (
                <div className={`text-xs p-3 rounded-xl border mt-2 ${
                  stakeStatus.type === "success" 
                    ? "bg-emerald-500/10 border-emerald-500/20 text-emerald-400" 
                    : "bg-red-500/10 border-red-500/20 text-red-400"
                }`}>
                  {stakeStatus.msg}
                </div>
              )}
            </div>

            {/* Emir Gir (Place Order) Card */}
            <div className="bg-zinc-900/50 border border-zinc-800 rounded-3xl p-6 flex flex-col gap-4 relative overflow-hidden">
              <h3 className="text-base font-semibold text-white flex items-center gap-2">
                <Coins className="h-5 w-5 text-emerald-400" /> Emir Gir (Place Order)
              </h3>
              <p className="text-xs text-zinc-400">
                Piyasaya Alış veya Satış limiti emri ekleyin.
              </p>
              
              <form onSubmit={handlePlaceOrder} className="flex flex-col gap-3 mt-1">
                {/* Side Selection */}
                <div className="grid grid-cols-2 gap-2 p-1 bg-zinc-950 rounded-xl border border-zinc-850">
                  <button
                    type="button"
                    onClick={() => setOrderSide("Buy")}
                    className={`py-2 text-xs font-bold rounded-lg transition-colors ${
                      orderSide === "Buy" 
                        ? "bg-emerald-500 text-zinc-950" 
                        : "text-zinc-400 hover:text-white"
                    }`}
                  >
                    ALIŞ (BUY)
                  </button>
                  <button
                    type="button"
                    onClick={() => setOrderSide("Sell")}
                    className={`py-2 text-xs font-bold rounded-lg transition-colors ${
                      orderSide === "Sell" 
                        ? "bg-red-500 text-white" 
                        : "text-zinc-400 hover:text-white"
                    }`}
                  >
                    SATIŞ (SELL)
                  </button>
                </div>

                {/* Model Selection */}
                <div className="grid grid-cols-2 gap-2">
                  <div className="flex flex-col gap-1">
                    <label className="text-[10px] text-zinc-500 font-bold uppercase">Sağlayıcı</label>
                    <select
                      value={orderProvider}
                      onChange={(e) => {
                        setOrderProvider(e.target.value);
                        setOrderModel(e.target.value === "OpenAI" ? "gpt-4o" : "claude-3-5-sonnet");
                      }}
                      className="bg-zinc-950 border border-zinc-850 rounded-xl px-3 py-2 text-xs text-white focus:border-emerald-500/50 outline-none"
                    >
                      <option value="OpenAI">OpenAI</option>
                      <option value="Anthropic">Anthropic</option>
                    </select>
                  </div>
                  <div className="flex flex-col gap-1">
                    <label className="text-[10px] text-zinc-500 font-bold uppercase">Model</label>
                    <select
                      value={orderModel}
                      onChange={(e) => setOrderModel(e.target.value)}
                      className="bg-zinc-950 border border-zinc-850 rounded-xl px-3 py-2 text-xs text-white focus:border-emerald-500/50 outline-none"
                    >
                      {orderProvider === "OpenAI" ? (
                        <>
                          <option value="gpt-4o">gpt-4o</option>
                          <option value="gpt-4-turbo">gpt-4-turbo</option>
                        </>
                      ) : (
                        <>
                          <option value="claude-3-5-sonnet">claude-3-5-sonnet</option>
                          <option value="claude-3-opus">claude-3-opus</option>
                        </>
                      )}
                    </select>
                  </div>
                </div>

                {/* Amount and Price */}
                <div className="grid grid-cols-2 gap-2">
                  <div className="flex flex-col gap-1">
                    <label className="text-[10px] text-zinc-500 font-bold uppercase">Miktar (Token)</label>
                    <input
                      type="number"
                      value={orderTokens}
                      onChange={(e) => setOrderTokens(e.target.value)}
                      className="bg-zinc-950 border border-zinc-850 rounded-xl px-3 py-2 text-xs text-white focus:border-emerald-500/50 outline-none font-mono"
                    />
                  </div>
                  <div className="flex flex-col gap-1">
                    <label className="text-[10px] text-zinc-500 font-bold uppercase">Fiyat ($/1k)</label>
                    <input
                      type="number"
                      step="0.0001"
                      value={orderPrice}
                      onChange={(e) => setOrderPrice(e.target.value)}
                      className="bg-zinc-950 border border-zinc-850 rounded-xl px-3 py-2 text-xs text-white focus:border-emerald-500/50 outline-none font-mono"
                    />
                  </div>
                </div>

                <button
                  type="submit"
                  className="w-full bg-emerald-500 hover:bg-emerald-600 transition-colors text-zinc-950 font-bold py-2.5 rounded-xl text-xs flex items-center justify-center gap-1.5 shadow-lg shadow-emerald-500/10 mt-1"
                >
                  Emir Gönder
                </button>
              </form>

              {orderStatus.msg && (
                <div className={`text-xs p-3 rounded-xl border mt-1 ${
                  orderStatus.type === "success" 
                    ? "bg-emerald-500/10 border-emerald-500/20 text-emerald-400" 
                    : "bg-red-500/10 border-red-500/20 text-red-400"
                }`}>
                  {orderStatus.msg}
                </div>
              )}
            </div>

            {/* Aktif Emirlerim Card */}
            <div className="bg-zinc-900/50 border border-zinc-800 rounded-3xl p-6 flex flex-col gap-4">
              <h3 className="text-base font-semibold text-white flex items-center gap-2">
                <Database className="h-5 w-5 text-emerald-400" /> Aktif Emirlerim
              </h3>
              <div className="flex flex-col gap-2 max-h-48 overflow-y-auto pr-1">
                {!wallet?.my_orders || wallet.my_orders.length === 0 ? (
                  <p className="text-xs text-zinc-500 py-2 text-center">Aktif emriniz bulunmuyor.</p>
                ) : (
                  wallet.my_orders.map((order) => (
                    <div key={order.id} className="bg-zinc-950/40 border border-zinc-850 rounded-xl p-3 flex justify-between items-center text-xs">
                      <div>
                        <span className={`px-1.5 py-0.5 rounded text-[10px] font-bold mr-1.5 ${
                          order.side === "Buy" ? "bg-emerald-500/10 text-emerald-400" : "bg-red-500/10 text-red-400"
                        }`}>
                          {order.side === "Buy" ? "AL" : "SAT"}
                        </span>
                        <span className="font-mono text-zinc-300 font-semibold">{order.model}</span>
                      </div>
                      <div className="text-right">
                        <div className="font-mono text-white">{order.token_amount.toLocaleString()} token</div>
                        <div className="font-mono text-emerald-400">${order.price_per_1k.toFixed(6)}/1k</div>
                      </div>
                    </div>
                  ))
                )}
              </div>
            </div>

          </div>

          {/* Right Column: Live Order Book & Fintech webhook deposits */}
          <div className="flex flex-col gap-8 lg:col-span-2">
            
            {/* Live Order Book */}
            <div className="bg-zinc-900/50 border border-zinc-800 rounded-3xl p-6 flex flex-col gap-4">
              <div className="flex items-center justify-between">
                <h3 className="text-base font-semibold text-white flex items-center gap-2">
                  <Database className="h-5 w-5 text-emerald-400" /> Canlı Kota Emir Defteri
                </h3>
                <span className="flex items-center gap-1.5 text-xs text-emerald-400 font-medium px-2 py-0.5 rounded border border-emerald-400/20 bg-emerald-400/10">
                  <span className="h-1.5 w-1.5 rounded-full bg-emerald-400 animate-ping"></span> Canlı (2s)
                </span>
              </div>
              
              <div className="overflow-x-auto">
                <table className="w-full text-left text-sm">
                  <thead>
                    <tr className="border-b border-zinc-800 text-zinc-400 text-xs uppercase tracking-wider">
                      <th className="pb-3">Tip</th>
                      <th className="pb-3">Provider</th>
                      <th className="pb-3">Model</th>
                      <th className="pb-3 text-right">Miktar (Token)</th>
                      <th className="pb-3 text-right">Fiyat ($/1k)</th>
                      <th className="pb-3 text-right">İşlem</th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-zinc-800/50">
                    {orders.length === 0 ? (
                      <tr>
                        <td colSpan={6} className="py-8 text-center text-zinc-500">
                          Aktif satış emri bulunmamaktadır. Piyasaya manuel emir girerek başlatabilirsiniz.
                        </td>
                      </tr>
                    ) : (
                      orders.map((order) => (
                        <tr key={order.id} className="hover:bg-zinc-900/30 transition-colors">
                          <td className="py-3">
                            <span className={`px-1.5 py-0.5 rounded text-[10px] font-bold ${
                              order.side === "Buy" ? "bg-emerald-500/10 text-emerald-400 border border-emerald-500/20" : "bg-red-500/10 text-red-400 border border-red-500/20"
                            }`}>
                              {order.side === "Buy" ? "AL" : "SAT"}
                            </span>
                          </td>
                          <td className="py-3">
                            <span className={`px-2 py-0.5 rounded text-xs font-semibold ${
                              order.provider === "OpenAI" 
                                ? "bg-purple-500/10 text-purple-400 border border-purple-500/20" 
                                : order.provider === "Anthropic"
                                  ? "bg-orange-500/10 text-orange-400 border border-orange-500/20"
                                  : "bg-zinc-500/10 text-zinc-400 border border-zinc-500/20"
                            }`}>
                              {order.provider}
                            </span>
                          </td>
                          <td className="py-3 font-mono font-medium text-white">{order.model}</td>
                          <td className="py-3 text-right font-mono text-zinc-300">{order.token_amount.toLocaleString()}</td>
                          <td className="py-3 text-right font-mono text-emerald-400 font-semibold">${order.price_per_1k.toFixed(6)}</td>
                          <td className="py-3 text-right">
                            {order.api_key === wallet?.api_key ? (
                              <span className="text-xs text-zinc-500 font-medium">Senin</span>
                            ) : (
                              <button
                                onClick={() => handleMatchOrder(order.id)}
                                disabled={matchLoadingId === order.id}
                                className={`text-[11px] font-bold px-2.5 py-1 rounded-lg transition-colors ${
                                  order.side === "Buy"
                                    ? "bg-red-500 hover:bg-red-600 text-white"
                                    : "bg-emerald-500 hover:bg-emerald-600 text-zinc-950"
                                }`}
                              >
                                {matchLoadingId === order.id ? "..." : (order.side === "Buy" ? "Satış Yap" : "Satın Al")}
                              </button>
                            )}
                          </td>
                        </tr>
                      ))
                    )}
                  </tbody>
                </table>
              </div>
            </div>

            {/* Fintech Simulator Cards */}
            <div className="bg-zinc-900/50 border border-zinc-800 rounded-3xl p-6 flex flex-col gap-4">
              <h3 className="text-base font-semibold text-white flex items-center gap-2">
                <CreditCard className="h-5 w-5 text-emerald-400" /> Bakiye Yükleme Simülatörü
              </h3>
              
              <div className="grid grid-cols-1 md:grid-cols-2 gap-6 mt-1">
                {/* Stripe Simulator */}
                <div className="bg-zinc-950/40 border border-zinc-800/80 rounded-2xl p-5 flex flex-col gap-3">
                  <div>
                    <h4 className="text-sm font-semibold text-white flex items-center gap-1.5">
                      LemonSqueezy / Stripe Webhook
                    </h4>
                    <p className="text-xs text-zinc-400 mt-1">
                      Platforma kredi kartı ile bakiye yükleme işlemini simüle eder.
                    </p>
                  </div>
                  <div className="flex gap-2">
                    <input
                      type="number"
                      placeholder="Miktar"
                      value={stripeAmount}
                      onChange={(e) => setStripeAmount(e.target.value)}
                      className="bg-zinc-950 border border-zinc-850 rounded-xl px-3 py-2 text-sm outline-none w-24 text-white"
                    />
                    <button
                      onClick={handleStripeDeposit}
                      disabled={stripeLoading || !wallet}
                      className="flex-1 bg-zinc-900 hover:bg-zinc-800 border border-zinc-700 text-white rounded-xl text-xs font-semibold py-2.5 transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center justify-center gap-1.5"
                    >
                      {stripeLoading && <span className="h-3 w-3 border-2 border-white/20 border-t-white rounded-full animate-spin"></span>}
                      Bakiye Yükle (Stripe)
                    </button>
                  </div>
                </div>

                {/* Solana Mock Simulator */}
                <div className="bg-zinc-950/40 border border-zinc-800/80 rounded-2xl p-5 flex flex-col gap-3">
                  <div>
                    <h4 className="text-sm font-semibold text-white flex items-center gap-1.5">
                      Solana Zincir İçi USDC
                    </h4>
                    <p className="text-xs text-zinc-400 mt-1">
                      Solana işlem numarası (TX) girerek USDC doğrulamasını tetikler.
                    </p>
                  </div>
                  <div className="flex flex-col gap-2">
                    <input
                      type="text"
                      placeholder="İşlem Hash'i (TX Hash)"
                      value={cryptoTx}
                      onChange={(e) => setCryptoTx(e.target.value)}
                      className="bg-zinc-950 border border-zinc-850 rounded-xl px-3 py-2 text-sm outline-none text-white font-mono text-xs"
                    />
                    <div className="flex gap-2">
                      <input
                        type="number"
                        placeholder="USDC"
                        value={cryptoAmount}
                        onChange={(e) => setCryptoAmount(e.target.value)}
                        className="bg-zinc-950 border border-zinc-850 rounded-xl px-3 py-2 text-sm outline-none w-20 text-white"
                      />
                      <button
                        onClick={handleCryptoVerify}
                        disabled={cryptoLoading || !wallet}
                        className="flex-1 bg-zinc-900 hover:bg-zinc-800 border border-zinc-700 text-white rounded-xl text-xs font-semibold py-2.5 transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center justify-center gap-1.5"
                      >
                        {cryptoLoading && <span className="h-3 w-3 border-2 border-white/20 border-t-white rounded-full animate-spin"></span>}
                        İşlem Gönder (USDC)
                      </button>
                    </div>
                  </div>
                </div>
              </div>

              {depositStatus.msg && (
                <div className={`text-xs p-3 rounded-xl border ${
                  depositStatus.type === "success" 
                    ? "bg-emerald-500/10 border-emerald-500/20 text-emerald-400" 
                    : "bg-red-500/10 border-red-500/20 text-red-400"
                }`}>
                  {depositStatus.msg}
                </div>
              )}
            </div>

          </div>

        </div>

      </main>

      {/* Footer */}
      <footer className="border-t border-zinc-800 py-8 bg-zinc-900/20 mt-16 text-center text-xs text-zinc-500">
        <div className="max-w-7xl mx-auto px-4">
          <p>© {new Date().getFullYear()} Agent Grid Protocol. Tüm hakları saklıdır.</p>
          <p className="mt-1.5 text-zinc-600 font-mono">Borsa Port: 3000 | Arayüz Port: 3001</p>
        </div>
      </footer>
    </div>
  );
}
