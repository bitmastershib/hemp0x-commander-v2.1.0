// Ravencoin RPC client for Commander multi-chain support.
//
// Mirrors the Hemp0x rpc.rs module but targets `ravend` on port 8766
// using the standard Ravencoin data directory and config file.
//
// Copyright (c) 2024-2025 The Hemp0x developers
// Distributed under the MIT software license.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use base64::prelude::*;
use serde::Serialize;

use crate::modules::rpc::{RpcAuthMode, RpcResult};
use crate::modules::rvn_models::{
    RvnDashboardData, RvnNodeInfo, RvnTxItem, RvnWalletInfo,
};

// ─── Ravencoin Path Helpers ──────────────────────────────────────────────────

/// Returns the OS-standard Ravencoin data directory.
/// Windows: %APPDATA%/Raven
/// Linux:   ~/.raven
/// macOS:   ~/Library/Application Support/Raven
pub fn rvn_data_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA")
            .map_err(|_| "APPDATA environment variable not set".to_string())?;
        Ok(PathBuf::from(appdata).join("Raven"))
    }

    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
        Ok(home.join("Library").join("Application Support").join("Raven"))
    }

    #[cfg(target_os = "linux")]
    {
        let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
        Ok(home.join(".raven"))
    }
}

/// Returns the path to raven.conf inside the data directory.
pub fn rvn_config_path() -> Result<PathBuf, String> {
    Ok(rvn_data_dir()?.join("raven.conf"))
}

/// Parse a simple key=value config file (same format as hemp.conf / raven.conf).
fn parse_rvn_config(path: &std::path::Path) -> Result<std::collections::HashMap<String, String>, String> {
    if !path.exists() {
        return Ok(std::collections::HashMap::new());
    }
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let mut config = std::collections::HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            config.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    Ok(config)
}

/// Ensure a minimal raven.conf exists for RPC communication.
pub fn ensure_rvn_config() -> Result<PathBuf, String> {
    let config_path = rvn_config_path()?;
    let data_dir = rvn_data_dir()?;

    // Create data directory if it doesn't exist
    if !data_dir.exists() {
        fs::create_dir_all(&data_dir)
            .map_err(|e| format!("Failed to create Ravencoin data directory: {e}"))?;
    }

    // Only create config if it doesn't exist at all
    if !config_path.exists() {
        let default_config = "# Ravencoin configuration (auto-created by Hemp0x Commander)\n\
                              server=1\n\
                              rpcallowip=127.0.0.1\n\
                              rpcport=8766\n\
                              listen=1\n\
                              txindex=1\n\
                              assetindex=1\n";
        fs::write(&config_path, default_config)
            .map_err(|e| format!("Failed to write raven.conf: {e}"))?;
    }

    Ok(config_path)
}

// ─── RPC Connection ──────────────────────────────────────────────────────────

fn rvn_rpc_url() -> Result<String, String> {
    let config = parse_rvn_config(&rvn_config_path()?)?;
    let port = config.get("rpcport").map(|v| v.as_str()).unwrap_or("8766");
    Ok(format!("http://127.0.0.1:{port}"))
}

fn rvn_rpc_auth() -> Result<(String, RpcAuthMode), String> {
    let dir = rvn_data_dir()?;
    let config_path = rvn_config_path()?;

    // Try legacy rpcuser/rpcpassword first
    if config_path.exists() {
        let config = parse_rvn_config(&config_path)?;
        let user = config.get("rpcuser");
        let pass = config.get("rpcpassword");
        if let (Some(u), Some(p)) = (user, pass) {
            let auth = format!("{u}:{p}");
            return Ok((
                format!("Basic {}", BASE64_STANDARD.encode(auth.as_bytes())),
                RpcAuthMode::LegacyUserpass,
            ));
        }
    }

    // Try cookie auth
    let cookie_path = dir.join(".cookie");
    if cookie_path.exists() {
        let cookie = fs::read_to_string(&cookie_path)
            .map_err(|e| format!("Failed to read Ravencoin RPC cookie: {e}"))?;
        let cookie = cookie.trim().to_string();
        if !cookie.is_empty() && cookie.contains(':') {
            return Ok((
                format!("Basic {}", BASE64_STANDARD.encode(cookie.as_bytes())),
                RpcAuthMode::Cookie,
            ));
        }
    }

    Err("Ravencoin RPC authentication unavailable: no cookie file and no rpcuser/rpcpassword in raven.conf".to_string())
}

struct RvnRpcContext {
    url: String,
    auth: String,
    auth_mode: RpcAuthMode,
}

fn rvn_rpc_context() -> Result<RvnRpcContext, String> {
    let url = rvn_rpc_url()?;
    let (auth, auth_mode) = rvn_rpc_auth()?;
    Ok(RvnRpcContext {
        url,
        auth,
        auth_mode,
    })
}

fn send_rvn_rpc_request(
    url: &str,
    auth: &str,
    method: &str,
    params: &[serde_json::Value],
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({
        "jsonrpc": "1.0",
        "id": "commander-rvn",
        "method": method,
        "params": params,
    });

    let response_result = ureq::AgentBuilder::new()
        .timeout_connect(connect_timeout)
        .timeout_read(read_timeout)
        .build()
        .post(url)
        .set("Authorization", auth)
        .set("Content-Type", "application/json")
        .send_json(&body);

    let raw: serde_json::Value = match response_result {
        Ok(resp) => resp
            .into_json()
            .map_err(|e| format!("RVN RPC parse error ({method}): {e}"))?,
        Err(ureq::Error::Status(401, _)) => {
            return Err(format!(
                "Ravencoin RPC authentication rejected ({method}). Check raven.conf credentials."
            ));
        }
        Err(ureq::Error::Status(status, resp)) => resp.into_json().map_err(|e| {
            format!("RVN RPC HTTP {status} with unreadable response body ({method}): {e}")
        })?,
        Err(e) => {
            return Err(format!("RVN RPC transport error ({method}): {e}"));
        }
    };

    // Parse RPC response envelope
    if let Some(err) = raw.get("error").filter(|e| !e.is_null()) {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown RPC error");
        return Err(format!("RVN RPC error ({method}): {msg}"));
    }

    Ok(raw
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

/// Generic Ravencoin RPC call.
pub(crate) fn call_rvn_rpc(
    method: &str,
    params: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    let ctx = rvn_rpc_context()?;
    send_rvn_rpc_request(
        &ctx.url,
        &ctx.auth,
        method,
        params,
        Duration::from_secs(5),
        Duration::from_secs(15),
    )
}

/// Generic Ravencoin RPC call with custom timeouts.
pub(crate) fn call_rvn_rpc_with_timeouts(
    method: &str,
    params: &[serde_json::Value],
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<serde_json::Value, String> {
    let ctx = rvn_rpc_context()?;
    send_rvn_rpc_request(
        &ctx.url,
        &ctx.auth,
        method,
        params,
        connect_timeout,
        read_timeout,
    )
}

fn rvn_rpc_result(method: &str, params: &[serde_json::Value]) -> RpcResult {
    match call_rvn_rpc(method, params) {
        Ok(data) => RpcResult {
            success: true,
            data,
            error: String::new(),
        },
        Err(e) => RpcResult {
            success: false,
            data: serde_json::Value::Null,
            error: e,
        },
    }
}

// ─── Allowed Methods ─────────────────────────────────────────────────────────

const RVN_ALLOWED_METHODS: &[&str] = &[
    "getassetdata",
    "getbestblockhash",
    "getblockchaininfo",
    "getblockcount",
    "getinfo",
    "getmempoolinfo",
    "getmininginfo",
    "getnetworkinfo",
    "getpeerinfo",
    "getwalletinfo",
    "listassets",
    "listmyassets",
    "listtransactions",
    "listunspent",
    "estimatesmartfee",
    "getnewaddress",
    "getrawchangeaddress",
    "validateaddress",
    "getaddressbalance",
    "getaddressdeltas",
    "getaddressutxos",
    "listaddressgroupings",
];

// ─── Tauri Commands ──────────────────────────────────────────────────────────

#[tauri::command]
pub async fn rvn_rpc_get_blockchain_info() -> RpcResult {
    tauri::async_runtime::spawn_blocking(|| rvn_rpc_result("getblockchaininfo", &[]))
        .await
        .unwrap_or_else(|e| RpcResult {
            success: false,
            data: serde_json::Value::Null,
            error: format!("RVN blockchain info task failed: {e}"),
        })
}

#[tauri::command]
pub async fn rvn_rpc_get_network_info() -> RpcResult {
    tauri::async_runtime::spawn_blocking(|| rvn_rpc_result("getnetworkinfo", &[]))
        .await
        .unwrap_or_else(|e| RpcResult {
            success: false,
            data: serde_json::Value::Null,
            error: format!("RVN network info task failed: {e}"),
        })
}

#[tauri::command]
pub async fn rvn_rpc_get_wallet_info() -> RpcResult {
    tauri::async_runtime::spawn_blocking(|| rvn_rpc_result("getwalletinfo", &[]))
        .await
        .unwrap_or_else(|e| RpcResult {
            success: false,
            data: serde_json::Value::Null,
            error: format!("RVN wallet info task failed: {e}"),
        })
}

#[tauri::command]
pub async fn rvn_rpc_call(method: String, params: Vec<serde_json::Value>) -> RpcResult {
    if !RVN_ALLOWED_METHODS.contains(&method.as_str()) {
        return RpcResult {
            success: false,
            data: serde_json::Value::Null,
            error: format!(
                "RVN RPC method is not allowed through the read-only bridge: {method}"
            ),
        };
    }

    tauri::async_runtime::spawn_blocking(move || rvn_rpc_result(&method, &params))
        .await
        .unwrap_or_else(|e| RpcResult {
            success: false,
            data: serde_json::Value::Null,
            error: format!("RVN RPC task failed: {e}"),
        })
}

// ─── Dashboard Builder ──────────────────────────────────────────────────────

fn build_rvn_dashboard(
    info: &serde_json::Value,
    bc: &serde_json::Value,
    tx_raw: &serde_json::Value,
) -> Result<RvnDashboardData, String> {
    let peers = info["connections"].as_u64().unwrap_or(0);
    let diff_val = info["difficulty"].as_f64().unwrap_or(0.0);
    let balance_val = info["balance"].as_f64().unwrap_or(0.0);
    let pending_val = info["unconfirmed_balance"].as_f64().unwrap_or(0.0);
    let staked_val = info["immature_balance"].as_f64().unwrap_or(0.0);

    let unlocked_until = info["unlocked_until"].as_i64();
    let status = match unlocked_until {
        Some(0) => "LOCKED",
        Some(_) => "UNLOCKED",
        None => "UNENCRYPTED",
    };

    let b = bc["blocks"].as_u64().unwrap_or(0);
    let h = bc["headers"].as_u64().unwrap_or(0);
    let progress = bc["verificationprogress"].as_f64().unwrap_or(0.0);
    let initial_dl = bc["initialblockdownload"].as_bool().unwrap_or(false);
    let mtp = bc["mediantime"].as_i64().unwrap_or(0);
    let now = chrono::Local::now().timestamp();
    // Ravencoin has 60s block times; 5400s ≈ 90 blocks
    let is_synced = h > 0 && b >= h && progress >= 0.999 && !initial_dl && (now - mtp) < 5400;

    let node = RvnNodeInfo {
        state: "RUNNING".to_string(),
        blocks: b,
        headers: h,
        peers,
        diff: format!("{:.4}", diff_val),
        synced: is_synced,
    };

    let wallet = RvnWalletInfo {
        balance: format!("{:.3}", balance_val),
        pending: format!("{:.3}", pending_val),
        staked: format!("{:.3}", staked_val),
        status: status.to_string(),
    };

    let mut tx_vec: Vec<serde_json::Value> = tx_raw.as_array().unwrap_or(&Vec::new()).clone();
    tx_vec.sort_by(|a, b| {
        let time_a = a["time"].as_i64().unwrap_or(0);
        let time_b = b["time"].as_i64().unwrap_or(0);
        time_b.cmp(&time_a)
    });

    use chrono::{DateTime, Local, TimeZone};
    let txs: Vec<RvnTxItem> = tx_vec
        .iter()
        .take(50)
        .map(|tx| {
            let epoch = tx["time"].as_i64().unwrap_or(0);
            let dt: DateTime<Local> = Local
                .timestamp_opt(epoch, 0)
                .single()
                .unwrap_or_else(|| Local::now());
            let amount = tx["amount"].as_f64().unwrap_or(0.0);
            RvnTxItem {
                date: dt.format("%m/%d %H:%M").to_string(),
                tx_type: tx["category"].as_str().unwrap_or("unknown").to_string(),
                amount: format!("{:.7}", amount),
                conf: tx["confirmations"].as_u64().unwrap_or(0),
                txid: tx["txid"].as_str().unwrap_or("-").to_string(),
            }
        })
        .collect();

    Ok(RvnDashboardData { node, wallet, tx: txs })
}

#[tauri::command]
pub async fn rvn_rpc_dashboard() -> Result<RvnDashboardData, String> {
    tauri::async_runtime::spawn_blocking(rvn_rpc_dashboard_blocking)
        .await
        .map_err(|e| format!("RVN Dashboard RPC task failed: {e}"))?
}

fn rvn_rpc_dashboard_blocking() -> Result<RvnDashboardData, String> {
    let fast_connect = Duration::from_millis(800);
    let fast_read = Duration::from_millis(2500);

    let info = call_rvn_rpc_with_timeouts("getinfo", &[], fast_connect, fast_read)?;
    let bc = call_rvn_rpc_with_timeouts("getblockchaininfo", &[], fast_connect, fast_read)?;
    let tx_params = [
        serde_json::Value::String("*".to_string()),
        serde_json::Value::Number(serde_json::value::Number::from(100)),
    ];
    let tx_raw = call_rvn_rpc_with_timeouts("listtransactions", &tx_params, fast_connect, fast_read)?;

    build_rvn_dashboard(&info, &bc, &tx_raw)
}

// ─── RVN Config Status ──────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct RvnConfigStatus {
    pub data_dir: String,
    pub config_path: String,
    pub config_exists: bool,
    pub data_dir_exists: bool,
}

#[tauri::command]
pub async fn rvn_get_config_status() -> Result<RvnConfigStatus, String> {
    let data_dir = rvn_data_dir()?;
    let config_path = rvn_config_path()?;

    Ok(RvnConfigStatus {
        data_dir: data_dir.to_string_lossy().to_string(),
        config_path: config_path.to_string_lossy().to_string(),
        config_exists: config_path.exists(),
        data_dir_exists: data_dir.exists(),
    })
}

#[tauri::command]
pub async fn rvn_ensure_config() -> Result<String, String> {
    let path = ensure_rvn_config()?;
    Ok(path.to_string_lossy().to_string())
}
