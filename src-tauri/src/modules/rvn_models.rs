// Ravencoin data models for Commander multi-chain support.
// These mirror the Hemp0x models but carry RVN-specific context.
//
// Copyright (c) 2024-2025 The Hemp0x developers
// Distributed under the MIT software license.

use serde::{Deserialize, Serialize};

// ─── Ravencoin Dashboard ─────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct RvnNodeInfo {
    pub state: String,
    pub blocks: u64,
    pub headers: u64,
    pub peers: u64,
    pub diff: String,
    pub synced: bool,
}

#[derive(Serialize)]
pub struct RvnWalletInfo {
    pub balance: String,
    pub pending: String,
    pub staked: String,
    pub status: String,
}

#[derive(Serialize)]
pub struct RvnTxItem {
    pub date: String,
    #[serde(rename = "type")]
    pub tx_type: String,
    pub amount: String,
    pub conf: u64,
    pub txid: String,
}

#[derive(Serialize)]
pub struct RvnDashboardData {
    pub node: RvnNodeInfo,
    pub wallet: RvnWalletInfo,
    pub tx: Vec<RvnTxItem>,
}

// ─── Ravencoin Runtime ───────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct RvnRuntimeStatus {
    pub running: bool,
    pub rpc_ready: bool,
    pub version: String,
    pub data_dir: String,
    pub config_exists: bool,
}

#[derive(Serialize)]
pub struct RvnBinaryStatus {
    pub daemon_exists: bool,
    pub cli_exists: bool,
    pub daemon_path: String,
    pub cli_path: String,
}

// ─── Ravencoin Assets ────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct RvnAssetItem {
    pub name: String,
    pub balance: String,
    #[serde(rename = "type")]
    pub asset_type: String,
}

#[derive(Serialize)]
pub struct RvnAssetData {
    pub name: String,
    pub amount: f64,
    pub units: u8,
    pub reissuable: bool,
    pub has_ipfs: bool,
    pub ipfs_hash: String,
    pub block_height: u64,
}

// ─── Atomic Swap ─────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SwapOffer {
    pub swap_id: String,
    pub offer_code: String,
    pub secret_hash: String,
    pub offer_chain: String,
    pub offer_amount: f64,
    pub offer_asset: Option<String>,
    pub offer_address: String,
    pub want_chain: String,
    pub want_amount: f64,
    pub want_asset: Option<String>,
    pub want_address: String,
    pub htlc_address: String,
    pub htlc_txid: String,
    pub locktime: u64,
    pub created_at: u64,
    pub status: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SwapAcceptance {
    pub swap_id: String,
    pub accept_htlc_address: String,
    pub accept_htlc_txid: String,
    pub accept_locktime: u64,
    pub want_address: String,
    pub status: String,
    #[serde(default)]
    pub acceptor_refund_address: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct SwapStatus {
    pub swap_id: String,
    pub phase: String,
    pub offer_chain: String,
    pub offer_amount: f64,
    pub offer_asset: Option<String>,
    pub offer_address: String,
    pub want_chain: String,
    pub want_amount: f64,
    pub want_asset: Option<String>,
    pub want_address: String,
    pub offer_htlc_funded: bool,
    pub offer_htlc_confirmed: bool,
    pub accept_htlc_funded: bool,
    pub accept_htlc_confirmed: bool,
    pub secret_revealed: bool,
    pub completed: bool,
    pub error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct SwapSummary {
    pub swap_id: String,
    pub phase: String,
    pub offer_chain: String,
    pub offer_amount: f64,
    pub offer_asset: Option<String>,
    pub offer_address: String,
    pub want_chain: String,
    pub want_amount: f64,
    pub want_asset: Option<String>,
    pub want_address: String,
    pub created_at: u64,
}

// ─── Ravencoin Transaction History ───────────────────────────────────────────

#[derive(Serialize)]
pub struct RvnTransactionHistoryItem {
    pub txid: String,
    pub date: String,
    #[serde(rename = "type")]
    pub tx_type: String,
    pub amount: String,
    pub confirmations: u64,
    pub address: Option<String>,
    pub asset: Option<String>,
}

#[derive(Serialize)]
pub struct RvnTransactionHistoryResult {
    pub items: Vec<RvnTransactionHistoryItem>,
    pub total: usize,
    pub has_more: bool,
}
