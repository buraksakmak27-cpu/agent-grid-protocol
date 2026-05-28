"use client";
import React, { FC, useMemo } from 'react';
import { ConnectionProvider, WalletProvider } from '@solana/wallet-adapter-react';
import { PhantomWalletAdapter } from '@solana/wallet-adapter-wallets';
import { WalletModalProvider } from '@solana/wallet-adapter-react-ui';

// Solana Wallet Adapter CSS stillerini dahil ediyoruz
import '@solana/wallet-adapter-react-ui/styles.css';

// Disable standard wallet adapter registration and localStorage tracking of walletName
if (typeof window !== 'undefined') {
    // Disable standard wallet adapter auto-detection (MetaMask, Solflare standard, Injected, etc.)
    const originalAddEventListener = window.addEventListener;
    (window as any).addEventListener = function (type: string, listener: any, options?: any) {
        if (type === 'wallet-standard:register-wallet') {
            return;
        }
        return originalAddEventListener.call(window, type, listener, options);
    };

    // Override navigator.wallets to prevent legacy standard wallet detection
    try {
        Object.defineProperty(window.navigator, 'wallets', {
            value: [],
            writable: false,
            configurable: true
        });
    } catch (e) {
        // ignore
    }

    // Disable localStorage tracking of walletName
    const originalGetItem = window.localStorage.getItem;
    const originalSetItem = window.localStorage.setItem;
    const originalRemoveItem = window.localStorage.removeItem;

    window.localStorage.getItem = function (key: string) {
        if (key === 'walletName' || key === 'walletAdapter' || key === '') return null;
        return originalGetItem.call(window.localStorage, key);
    };

    window.localStorage.setItem = function (key: string, value: string) {
        if (key === 'walletName' || key === 'walletAdapter' || key === '') return;
        return originalSetItem.call(window.localStorage, key, value);
    };

    window.localStorage.removeItem = function (key: string) {
        if (key === 'walletName' || key === 'walletAdapter' || key === '') return;
        return originalRemoveItem.call(window.localStorage, key);
    };
}

export const WalletConnectionProvider: FC<{ children: React.ReactNode }> = ({ children }) => {
    // Solana Mainnet (Dinamik RPC konfigurasyonu veya varsayılan public RPC)
    const endpoint = process.env.NEXT_PUBLIC_SOLANA_RPC_URL || "https://api.mainnet-beta.solana.com";

    const wallets = useMemo(
        () => [
            new PhantomWalletAdapter(),
        ],
        []
    );

    return (
        <ConnectionProvider endpoint={endpoint}>
            <WalletProvider wallets={wallets} autoConnect={false} localStorageKey={null as any} useStandardWalletAdapters={false}>
                <WalletModalProvider>
                    {children}
                </WalletModalProvider>
            </WalletProvider>
        </ConnectionProvider>
    );
};

