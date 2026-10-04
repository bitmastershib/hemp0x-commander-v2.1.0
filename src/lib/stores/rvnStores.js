import { writable } from 'svelte/store';

export const rvnNodeStatus = writable({
    online: false, 
    version: "--", 
    connections: 0,
    headers: 0, 
    blocks: 0, 
    verificationProgress: 0, 
    error: null
});

export const rvnWalletInfo = writable({
    balance: "--", 
    unconfirmed: 0.0, 
    immature: 0.0,
    transactions: [], 
    status: "--"
});

export const rvnNetworkInfo = writable({
    chain: "mainnet", 
    difficulty: 0, 
    networkHashps: 0
});

export const rvnDaemonRuntime = writable({
    running: false, 
    rpc_ready: false,
    version: "--",
    data_dir: "",
    config_exists: false
});
