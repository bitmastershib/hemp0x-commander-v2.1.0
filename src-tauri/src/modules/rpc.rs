use std::fs;
use std::time::Duration;

use base64::prelude::*;
use serde::Serialize;

use crate::modules::files::{
    config_path, data_dir, ensure_config, load_app_settings_impl, parse_config,
};
use crate::modules::models::DashboardData;

#[derive(Debug, Serialize)]
pub struct RpcResult {
    pub success: bool,
    pub data: serde_json::Value,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RpcAuthMode {
    Cookie,
    LegacyUserpass,
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcAuthInfo {
    pub auth_mode: RpcAuthMode,
    pub cookie_exists: bool,
    pub legacy_credentials_exist: bool,
    pub warning: String,
}

fn rpc_url() -> Result<String, String> {
    let config = parse_config(&config_path()?)?;
    let port = config.get("rpcport").map(|v| v.as_str()).unwrap_or("42068");
    Ok(format!("http://127.0.0.1:{port}"))
}

fn rpc_auth() -> Result<(String, RpcAuthMode), String> {
    let dir = data_dir()?;
    let cookie_path = dir.join(".cookie");
    rpc_auth_for_paths(&cookie_path, &config_path()?)
}

fn rpc_auth_for_paths(
    cookie_path: &std::path::Path,
    config_path: &std::path::Path,
) -> Result<(String, RpcAuthMode), String> {
    let config = if config_path.exists() {
        Some(parse_config(config_path)?)
    } else {
        None
    };

    if let Some(config) = config.as_ref() {
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

    if cookie_path.exists() {
        let cookie = fs::read_to_string(cookie_path)
            .map_err(|e| format!("Failed to read RPC cookie: {e}"))?;
        let cookie = cookie.trim().to_string();
        if !cookie.is_empty() && cookie.contains(':') {
            return Ok((
                format!("Basic {}", BASE64_STANDARD.encode(cookie.as_bytes())),
                RpcAuthMode::Cookie,
            ));
        }
    }

    if config.is_none() {
        return Err(
            "RPC authentication unavailable: no cookie file and no rpcuser/rpcpassword in hemp.conf"
                .to_string(),
        );
    }

    Err(
        "RPC authentication unavailable: no cookie file and no rpcuser/rpcpassword in hemp.conf"
            .to_string(),
    )
}

#[tauri::command]
pub fn get_rpc_auth_status() -> RpcAuthInfo {
    let (cookie_exists, legacy_exists) = detect_auth_sources();

    let (auth_mode, warning) = if legacy_exists {
        (
            RpcAuthMode::LegacyUserpass,
            "Legacy RPC password auth is active. Cookie auth is recommended for Commander v2 and Core Next."
                .to_string(),
        )
    } else if cookie_exists {
        (RpcAuthMode::Cookie, String::new())
    } else {
        (RpcAuthMode::Unavailable, String::new())
    };

    RpcAuthInfo {
        auth_mode,
        cookie_exists,
        legacy_credentials_exist: legacy_exists,
        warning,
    }
}

fn detect_auth_sources() -> (bool, bool) {
    let cookie_exists = data_dir()
        .map(|dir| {
            let cp = dir.join(".cookie");
            cp.exists()
                && fs::read_to_string(&cp)
                    .map(|c| !c.trim().is_empty() && c.contains(':'))
                    .unwrap_or(false)
        })
        .unwrap_or(false);

    let legacy_exists = config_path()
        .ok()
        .and_then(|p| parse_config(&p).ok())
        .map(|config| config.contains_key("rpcuser") && config.contains_key("rpcpassword"))
        .unwrap_or(false);

    (cookie_exists, legacy_exists)
}

pub(crate) struct RpcContext {
    url: String,
    auth: String,
    auth_mode: RpcAuthMode,
}

pub(crate) fn rpc_context() -> Result<RpcContext, String> {
    let url = rpc_url()?;
    let (auth, auth_mode) = rpc_auth()?;
    Ok(RpcContext {
        url,
        auth,
        auth_mode,
    })
}

#[derive(Debug)]
enum RpcHttpFailure {
    AuthRejected,
    Other(String),
}

impl RpcHttpFailure {
    fn into_message(self, method: &str) -> String {
        match self {
            RpcHttpFailure::AuthRejected => format!(
                "RPC authentication rejected ({method}). Check rpcuser/rpcpassword or let Core refresh cookie auth."
            ),
            RpcHttpFailure::Other(e) => e,
        }
    }
}

fn parse_rpc_response(raw: serde_json::Value, method: &str) -> Result<serde_json::Value, String> {
    if let Some(err) = raw.get("error").filter(|e| !e.is_null()) {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown RPC error");
        return Err(format!("RPC error ({method}): {msg}"));
    }

    Ok(raw
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

fn send_rpc_request(
    url: &str,
    auth: &str,
    method: &str,
    params: &[serde_json::Value],
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<serde_json::Value, RpcHttpFailure> {
    let body = serde_json::json!({
        "jsonrpc": "1.0",
        "id": "commander",
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

    match response_result {
        Ok(resp) => resp
            .into_json()
            .map_err(|e| RpcHttpFailure::Other(format!("RPC parse error ({method}): {e}"))),
        Err(ureq::Error::Status(401, _)) => Err(RpcHttpFailure::AuthRejected),
        Err(ureq::Error::Status(status, resp)) => resp.into_json().map_err(|e| {
            RpcHttpFailure::Other(format!(
                "RPC HTTP {status} with unreadable response body ({method}): {e}"
            ))
        }),
        Err(e) => Err(RpcHttpFailure::Other(format!(
            "RPC transport error ({method}): {e}"
        ))),
    }
}

fn retry_cookie_auth_after_rejection(
    url: &str,
    previous_auth: &str,
    method: &str,
    params: &[serde_json::Value],
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<serde_json::Value, String> {
    std::thread::sleep(Duration::from_millis(350));
    let (fresh_auth, fresh_mode) = rpc_auth()?;
    if fresh_mode != RpcAuthMode::Cookie {
        return Err(format!(
            "RPC authentication rejected ({method}). Core no longer reports cookie auth as available."
        ));
    }

    if fresh_auth == previous_auth {
        return Err(format!(
            "RPC authentication rejected ({method}). Core may still be rotating the RPC cookie."
        ));
    }

    let raw = send_rpc_request(
        url,
        &fresh_auth,
        method,
        params,
        connect_timeout,
        read_timeout,
    )
    .map_err(|e| e.into_message(method))?;
    parse_rpc_response(raw, method)
}

impl RpcContext {
    pub(crate) fn call(
        &self,
        method: &str,
        params: &[serde_json::Value],
    ) -> Result<serde_json::Value, String> {
        self.call_with_timeouts(
            method,
            params,
            Duration::from_secs(5),
            Duration::from_secs(15),
        )
    }

    pub(crate) fn call_with_timeouts(
        &self,
        method: &str,
        params: &[serde_json::Value],
        connect_timeout: Duration,
        read_timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        match send_rpc_request(
            &self.url,
            &self.auth,
            method,
            params,
            connect_timeout,
            read_timeout,
        ) {
            Ok(raw) => parse_rpc_response(raw, method),
            Err(RpcHttpFailure::AuthRejected) if self.auth_mode == RpcAuthMode::Cookie => {
                retry_cookie_auth_after_rejection(
                    &self.url,
                    &self.auth,
                    method,
                    params,
                    connect_timeout,
                    read_timeout,
                )
            }
            Err(e) => Err(e.into_message(method)),
        }
    }
}

pub(crate) fn call_rpc(
    method: &str,
    params: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    let ctx = rpc_context()?;
    ctx.call(method, params)
}

pub(crate) fn call_rpc_with_timeouts(
    method: &str,
    params: &[serde_json::Value],
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<serde_json::Value, String> {
    let ctx = rpc_context()?;
    ctx.call_with_timeouts(method, params, connect_timeout, read_timeout)
}

pub(crate) fn is_rpc_transport_timeout_error(err: &str) -> bool {
    let lower = err.to_lowercase();
    (lower.contains("rpc transport error") || lower.contains("rpc transport"))
        && (lower.contains("timed out")
            || lower.contains("timeout")
            || lower.contains("status line"))
}

/// Call an RPC method targeting a specific named wallet via Core's
/// `/wallet/<name>` URL-path routing. Secrets (passphrases) are sent
/// in the JSON-RPC body, never in process arguments.
pub(crate) fn call_rpc_wallet(
    wallet_name: &str,
    method: &str,
    params: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    call_rpc_wallet_with_timeouts(
        wallet_name,
        method,
        params,
        Duration::from_secs(5),
        Duration::from_secs(15),
    )
}

pub(crate) fn call_rpc_wallet_with_timeouts(
    wallet_name: &str,
    method: &str,
    params: &[serde_json::Value],
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<serde_json::Value, String> {
    if wallet_name.is_empty() {
        return Err("Wallet name must not be empty for targeted RPC".to_string());
    }
    // Validate wallet name: same rules as Core wallet filenames
    if !wallet_name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "Invalid wallet name for RPC routing: {wallet_name}"
        ));
    }
    if wallet_name.len() > 64 {
        return Err("Wallet name too long for RPC routing".to_string());
    }

    let base_url = rpc_url()?;
    let wallet_url = format!("{base_url}/wallet/{wallet_name}");
    let (auth, auth_mode) = rpc_auth()?;
    let method_label = format!("{method} on wallet {wallet_name}");

    match send_rpc_request(
        &wallet_url,
        &auth,
        &method_label,
        params,
        connect_timeout,
        read_timeout,
    ) {
        Ok(raw) => parse_rpc_response(raw, &method_label),
        Err(RpcHttpFailure::AuthRejected) if auth_mode == RpcAuthMode::Cookie => {
            retry_cookie_auth_after_rejection(
                &wallet_url,
                &auth,
                &method_label,
                params,
                connect_timeout,
                read_timeout,
            )
        }
        Err(e) => Err(e.into_message(&method_label)),
    }
}

fn rpc_command_with_result(method: &str, params: &[serde_json::Value]) -> RpcResult {
    let result = if is_wallet_scoped_method(method) {
        call_active_wallet_or_default(method, params)
    } else {
        call_rpc(method, params)
    };
    match result {
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

fn is_wallet_scoped_method(method: &str) -> bool {
    matches!(method, "getwalletinfo" | "listtransactions" | "listunspent")
}

fn is_rpc_method_not_found(err: &str) -> bool {
    let lower = err.to_lowercase();
    lower.contains("method not found") || lower.contains("error code: -32601")
}

fn active_vault_wallet_name() -> Option<String> {
    load_app_settings_impl()
        .ok()
        .and_then(|settings| settings.active_vault_wallet_name)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

pub(crate) fn call_active_wallet_or_default(
    method: &str,
    params: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    if let Some(wallet_name) = active_vault_wallet_name() {
        match call_rpc_wallet(&wallet_name, method, params) {
            Ok(value) => Ok(value),
            Err(err)
                if method == "getwalletinfo"
                    && params.is_empty()
                    && is_rpc_method_not_found(&err) =>
            {
                let raw = crate::modules::commands::run_cli(&[
                    format!("-wallet={wallet_name}"),
                    String::from("getwalletinfo"),
                ])
                .map_err(|cli_err| {
                    format!("{err}. CLI wallet fallback also failed for '{wallet_name}': {cli_err}")
                })?;
                serde_json::from_str(&raw).map_err(|parse_err| {
                    format!(
                        "Could not parse CLI getwalletinfo for wallet '{wallet_name}': {parse_err}"
                    )
                })
            }
            Err(err) => Err(err),
        }
    } else {
        call_rpc(method, params)
    }
}

#[tauri::command]
pub async fn rpc_get_blockchain_info() -> RpcResult {
    tauri::async_runtime::spawn_blocking(|| rpc_command_with_result("getblockchaininfo", &[]))
        .await
        .unwrap_or_else(|e| RpcResult {
            success: false,
            data: serde_json::Value::Null,
            error: format!("Blockchain info task failed: {e}"),
        })
}

#[tauri::command]
pub async fn rpc_get_network_info() -> RpcResult {
    tauri::async_runtime::spawn_blocking(|| rpc_command_with_result("getnetworkinfo", &[]))
        .await
        .unwrap_or_else(|e| RpcResult {
            success: false,
            data: serde_json::Value::Null,
            error: format!("Network info task failed: {e}"),
        })
}

#[tauri::command]
pub async fn rpc_get_wallet_info() -> RpcResult {
    tauri::async_runtime::spawn_blocking(rpc_get_wallet_info_blocking)
        .await
        .unwrap_or_else(|e| RpcResult {
            success: false,
            data: serde_json::Value::Null,
            error: format!("Wallet info task failed: {e}"),
        })
}

fn rpc_get_wallet_info_blocking() -> RpcResult {
    match call_active_wallet_or_default("getwalletinfo", &[]) {
        Ok(mut data) => {
            if let Ok(migration_info) = call_active_wallet_or_default("getwalletmigrationinfo", &[])
            {
                merge_wallet_security_status(&mut data, &migration_info);
            }
            if let Some(wallet_name) = active_vault_wallet_name() {
                if let Some(wallet) = data.as_object_mut() {
                    wallet
                        .entry("walletname".to_string())
                        .or_insert_with(|| serde_json::Value::String(wallet_name.clone()));
                    wallet.insert(
                        "commander_active_wallet_name".to_string(),
                        serde_json::Value::String(wallet_name),
                    );
                }
            }
            RpcResult {
                success: true,
                data,
                error: String::new(),
            }
        }
        Err(e) => RpcResult {
            success: false,
            data: serde_json::Value::Null,
            error: e,
        },
    }
}

fn overlay_wallet_info(info: &mut serde_json::Value, wallet_info: &serde_json::Value) {
    let Some(info_obj) = info.as_object_mut() else {
        return;
    };
    for field in [
        "balance",
        "unconfirmed_balance",
        "immature_balance",
        "unlocked_until",
        "walletname",
    ] {
        if let Some(value) = wallet_info.get(field) {
            info_obj.insert(field.to_string(), value.clone());
        }
    }
}

fn merge_wallet_security_status(
    wallet_info: &mut serde_json::Value,
    migration_info: &serde_json::Value,
) {
    let Some(wallet) = wallet_info.as_object_mut() else {
        return;
    };
    for field in ["encrypted", "locked"] {
        if let Some(value) = migration_info.get(field).and_then(|value| value.as_bool()) {
            wallet.insert(field.to_string(), serde_json::Value::Bool(value));
        }
    }
}

const ALLOWED_METHODS: &[&str] = &[
    "getassetdata",
    "getbestblockhash",
    "getblockchaininfo",
    "getblockcount",
    "getinfo",
    "getmempoolinfo",
    "getmessaginginfo",
    "getmininginfo",
    "getnetworkinfo",
    "getpeerinfo",
    "getwalletinfo",
    "listassets",
    "listtransactions",
    "listunspent",
    "estimatesmartfee",
    "viewallmessages",
    "viewallmessagechannels",
    "viewchannelmessages",
    "getmessagetxid",
    "viewindexedmessagechannels",
    "rescanmessages",
];

#[tauri::command]
pub async fn rpc_call(method: String, params: Vec<serde_json::Value>) -> RpcResult {
    if !ALLOWED_METHODS.contains(&method.as_str()) {
        return RpcResult {
            success: false,
            data: serde_json::Value::Null,
            error: format!(
                "RPC method is not allowed through the generic read-only bridge: {method}"
            ),
        };
    }

    tauri::async_runtime::spawn_blocking(move || rpc_command_with_result(&method, &params))
        .await
        .unwrap_or_else(|e| RpcResult {
            success: false,
            data: serde_json::Value::Null,
            error: format!("RPC task failed: {e}"),
        })
}

pub(crate) fn build_dashboard_from_rpc(
    info: &serde_json::Value,
    bc: &serde_json::Value,
    tx_raw: &serde_json::Value,
) -> Result<DashboardData, String> {
    let blocks_info = info["blocks"].as_u64().unwrap_or(0);
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

    let b = bc["blocks"].as_u64().unwrap_or(blocks_info);
    let h = bc["headers"].as_u64().unwrap_or(0);
    let progress = bc["verificationprogress"].as_f64().unwrap_or(0.0);
    let initial_dl = bc["initialblockdownload"].as_bool().unwrap_or(false);
    let mtp = bc["mediantime"].as_i64().unwrap_or(0);
    let now = chrono::Local::now().timestamp();
    let is_synced = h > 0 && b >= h && progress >= 0.999 && !initial_dl && (now - mtp) < 5400;

    let node = crate::modules::models::NodeInfo {
        state: "RUNNING".to_string(),
        blocks: b,
        headers: h,
        peers,
        diff: format!("{:.4}", diff_val),
        synced: is_synced,
    };

    let wallet = crate::modules::models::WalletInfo {
        balance: format!("{:.3}", balance_val),
        pending: format!("{:.3}", pending_val),
        staked: format!("{:.3}", staked_val),
        status: status.to_string(),
    };

    let mut tx_vec: Vec<serde_json::Value> = tx_raw.as_array().unwrap_or(&Vec::new()).clone();
    tx_vec.sort_by(|a, b| {
        let time_a = a["time"].as_i64().unwrap_or(0);
        let time_b = b["time"].as_i64().unwrap_or(0);
        if time_a != time_b {
            return time_a.cmp(&time_b);
        }
        let cat_a = a["category"].as_str().unwrap_or("");
        let cat_b = b["category"].as_str().unwrap_or("");
        if cat_a == "send" && cat_b == "receive" {
            return std::cmp::Ordering::Less;
        }
        if cat_a == "receive" && cat_b == "send" {
            return std::cmp::Ordering::Greater;
        }
        cat_a.cmp(cat_b)
    });

    use chrono::{DateTime, Local, TimeZone};
    let mut txs = Vec::new();
    for tx in tx_vec.iter().rev().take(50) {
        let epoch = tx["time"].as_i64().unwrap_or(0);
        let dt: DateTime<Local> = Local
            .timestamp_opt(epoch, 0)
            .single()
            .unwrap_or_else(|| Local::now());
        let amount = tx["amount"].as_f64().unwrap_or(0.0);
        let item = crate::modules::models::TxItem {
            date: dt.format("%m/%d %H:%M").to_string(),
            tx_type: tx["category"].as_str().unwrap_or("unknown").to_string(),
            amount: format!("{:.7}", amount),
            conf: tx["confirmations"].as_u64().unwrap_or(0),
            txid: tx["txid"].as_str().unwrap_or("-").to_string(),
        };
        txs.push(item);
    }

    Ok(DashboardData {
        node,
        wallet,
        tx: txs,
    })
}

#[tauri::command]
pub async fn rpc_dashboard() -> Result<DashboardData, String> {
    tauri::async_runtime::spawn_blocking(rpc_dashboard_blocking)
        .await
        .map_err(|e| format!("Dashboard RPC task failed: {e}"))?
}

static SYNC_DASHBOARD_CACHE: std::sync::OnceLock<
    std::sync::Mutex<Option<(std::time::Instant, serde_json::Value, serde_json::Value)>>,
> = std::sync::OnceLock::new();

static LAST_PEER_CHECK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn boost_and_heal_sync_peers(ctx: &RpcContext) {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let prev = LAST_PEER_CHECK.load(std::sync::atomic::Ordering::Relaxed);
    if now_secs.saturating_sub(prev) < 20 {
        return;
    }
    LAST_PEER_CHECK.store(now_secs, std::sync::atomic::Ordering::Relaxed);

    let short_connect = Duration::from_millis(500);
    let short_read = Duration::from_millis(2000);

    let Ok(peers_val) = ctx.call_with_timeouts("getpeerinfo", &[], short_connect, short_read) else {
        return;
    };
    let Some(peers) = peers_val.as_array() else {
        return;
    };

    if peers.len() < 4 {
        for seed in [
            "66.94.124.100:42069",
            "154.38.164.123:42069",
            "[2605:a143:2288:4118::1]:42069",
            "[2605:a140:2288:616::1]:42069",
        ] {
            let _ = ctx.call_with_timeouts(
                "addnode",
                &[
                    serde_json::Value::String(seed.to_string()),
                    serde_json::Value::String("onetry".to_string()),
                ],
                short_connect,
                short_read,
            );
        }
    }

    if peers.len() >= 2 {
        let has_healthy_peer = peers.iter().any(|p| {
            p.get("pingwait").and_then(|v| v.as_f64()).unwrap_or(0.0) < 2.0
                && p.get("synced_headers").and_then(|v| v.as_i64()).unwrap_or(-1) > 0
        });
        if has_healthy_peer {
            for p in peers {
                let inflight_len = p
                    .get("inflight")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let pingwait = p.get("pingwait").and_then(|v| v.as_f64()).unwrap_or(0.0);
                if inflight_len > 0 && pingwait > 15.0 {
                    if let Some(addr) = p.get("addr").and_then(|v| v.as_str()) {
                        let _ = ctx.call_with_timeouts(
                            "disconnectnode",
                            &[serde_json::Value::String(addr.to_string())],
                            short_connect,
                            short_read,
                        );
                    }
                }
            }
        }
    }
}

pub(crate) fn rpc_dashboard_blocking() -> Result<DashboardData, String> {
    let _cfg = ensure_config()?;
    let ctx = rpc_context()?;

    let fast_connect = Duration::from_millis(1000);
    let fast_read = Duration::from_millis(8000);

    let bc = ctx.call_with_timeouts("getblockchaininfo", &[], fast_connect, fast_read)?;
    let b = bc["blocks"].as_u64().unwrap_or(0);
    let h = bc["headers"].as_u64().unwrap_or(0);
    let is_syncing = h > b + 10;

    if is_syncing {
        boost_and_heal_sync_peers(&ctx);
        let cache_mutex = SYNC_DASHBOARD_CACHE.get_or_init(|| std::sync::Mutex::new(None));
        if let Ok(guard) = cache_mutex.lock() {
            if let Some((ts, ref cached_info, ref cached_tx)) = *guard {
                if ts.elapsed() < Duration::from_secs(18) {
                    let mut info = cached_info.clone();
                    if let Some(obj) = info.as_object_mut() {
                        obj.insert("blocks".to_string(), serde_json::json!(b));
                        if let Some(diff) = bc.get("difficulty") {
                            obj.insert("difficulty".to_string(), diff.clone());
                        }
                    }
                    return build_dashboard_from_rpc(&info, &bc, cached_tx);
                }
            }
        }
    }

    let mut info = ctx.call_with_timeouts("getinfo", &[], fast_connect, fast_read)?;
    let tx_params = [
        serde_json::Value::String("*".to_string()),
        serde_json::Value::Number(serde_json::value::Number::from(100)),
    ];
    let tx_raw = if let Some(wallet_name) = active_vault_wallet_name() {
        let wallet_info = call_rpc_wallet_with_timeouts(
            &wallet_name,
            "getwalletinfo",
            &[],
            fast_connect,
            fast_read,
        )?;
        overlay_wallet_info(&mut info, &wallet_info);
        call_rpc_wallet_with_timeouts(
            &wallet_name,
            "listtransactions",
            &tx_params,
            fast_connect,
            fast_read,
        )?
    } else {
        if let Ok(wallet_info) =
            ctx.call_with_timeouts("getwalletinfo", &[], fast_connect, fast_read)
        {
            overlay_wallet_info(&mut info, &wallet_info);
        }
        ctx.call_with_timeouts("listtransactions", &tx_params, fast_connect, fast_read)?
    };

    if is_syncing {
        let cache_mutex = SYNC_DASHBOARD_CACHE.get_or_init(|| std::sync::Mutex::new(None));
        if let Ok(mut guard) = cache_mutex.lock() {
            *guard = Some((std::time::Instant::now(), info.clone(), tx_raw.clone()));
        }
    }

    build_dashboard_from_rpc(&info, &bc, &tx_raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_info(
        blocks: u64,
        connections: u64,
        difficulty: f64,
        balance: f64,
    ) -> serde_json::Value {
        serde_json::json!({
            "blocks": blocks,
            "connections": connections,
            "difficulty": difficulty,
            "balance": balance,
            "unconfirmed_balance": 0.0,
            "immature_balance": 0.0,
            "unlocked_until": 0,
        })
    }

    fn fixture_bc(blocks: u64, headers: u64, progress: f64, mediantime: i64) -> serde_json::Value {
        serde_json::json!({
            "blocks": blocks,
            "headers": headers,
            "verificationprogress": progress,
            "initialblockdownload": false,
            "mediantime": mediantime,
        })
    }

    fn fixture_tx() -> serde_json::Value {
        serde_json::json!([
            {"time": 1716150000, "category": "receive", "amount": 1.5, "confirmations": 3, "txid": "abc123"},
            {"time": 1716140000, "category": "send", "amount": -0.5, "confirmations": 10, "txid": "def456"},
        ])
    }

    #[test]
    fn build_dashboard_parses_running_state() {
        let info = fixture_info(100, 8, 1.5, 42.0);
        let bc = fixture_bc(100, 100, 1.0, chrono::Local::now().timestamp() - 100);
        let txs = fixture_tx();
        let result = build_dashboard_from_rpc(&info, &bc, &txs).unwrap();
        assert_eq!(result.node.state, "RUNNING");
        assert_eq!(result.node.blocks, 100);
        assert_eq!(result.node.peers, 8);
        assert!(result.node.synced);
        assert_eq!(result.wallet.balance, "42.000");
        assert_eq!(result.wallet.status, "LOCKED");
        assert_eq!(result.tx.len(), 2);
    }

    #[test]
    fn build_dashboard_detects_unsynced() {
        let info = fixture_info(50, 1, 0.5, 0.0);
        let bc = fixture_bc(50, 200, 0.5, 0);
        let txs = fixture_tx();
        let result = build_dashboard_from_rpc(&info, &bc, &txs).unwrap();
        assert!(!result.node.synced);
    }

    #[test]
    fn build_dashboard_handles_empty_tx_list() {
        let info = fixture_info(1, 0, 0.0, 0.0);
        let bc = fixture_bc(1, 1, 1.0, chrono::Local::now().timestamp() - 50);
        let txs = serde_json::json!([]);
        let result = build_dashboard_from_rpc(&info, &bc, &txs).unwrap();
        assert_eq!(result.tx.len(), 0);
    }

    #[test]
    fn wallet_status_uses_explicit_core_encryption_fields() {
        let mut wallet_info = serde_json::json!({
            "walletname": "wallet.dat",
            "balance": 42.0
        });
        let migration_info = serde_json::json!({
            "encrypted": true,
            "locked": true
        });

        merge_wallet_security_status(&mut wallet_info, &migration_info);

        assert_eq!(wallet_info["encrypted"], true);
        assert_eq!(wallet_info["locked"], true);
    }

    #[test]
    fn wallet_status_does_not_invent_missing_security_fields() {
        let mut wallet_info = serde_json::json!({
            "walletname": "wallet.dat"
        });
        merge_wallet_security_status(&mut wallet_info, &serde_json::json!({}));
        assert!(wallet_info.get("encrypted").is_none());
        assert!(wallet_info.get("locked").is_none());
    }

    #[test]
    fn overlay_wallet_info_replaces_default_wallet_fields() {
        let mut info = serde_json::json!({
            "blocks": 100,
            "connections": 8,
            "difficulty": 1.5,
            "balance": 0.0,
            "unconfirmed_balance": 0.0,
            "immature_balance": 0.0,
            "unlocked_until": 0,
        });
        let wallet_info = serde_json::json!({
            "walletname": "hemp0x-vault-main",
            "balance": 42.0,
            "unconfirmed_balance": 1.0,
            "immature_balance": 2.0,
            "unlocked_until": 123,
        });

        overlay_wallet_info(&mut info, &wallet_info);

        assert_eq!(info["walletname"], "hemp0x-vault-main");
        assert_eq!(info["balance"], 42.0);
        assert_eq!(info["unconfirmed_balance"], 1.0);
        assert_eq!(info["immature_balance"], 2.0);
        assert_eq!(info["unlocked_until"], 123);
    }

    #[test]
    fn allowed_methods_include_informational_rpcs() {
        assert!(ALLOWED_METHODS.contains(&"getinfo"));
        assert!(ALLOWED_METHODS.contains(&"getblockchaininfo"));
        assert!(ALLOWED_METHODS.contains(&"getnetworkinfo"));
        assert!(ALLOWED_METHODS.contains(&"getwalletinfo"));
        assert!(ALLOWED_METHODS.contains(&"listtransactions"));
        assert!(ALLOWED_METHODS.contains(&"listunspent"));
        assert!(ALLOWED_METHODS.contains(&"estimatesmartfee"));
        assert!(ALLOWED_METHODS.contains(&"getmessaginginfo"));
        assert!(ALLOWED_METHODS.contains(&"viewallmessages"));
        assert!(ALLOWED_METHODS.contains(&"viewallmessagechannels"));
        assert!(ALLOWED_METHODS.contains(&"viewchannelmessages"));
        assert!(ALLOWED_METHODS.contains(&"getmessagetxid"));
        assert!(ALLOWED_METHODS.contains(&"viewindexedmessagechannels"));
        assert!(ALLOWED_METHODS.contains(&"rescanmessages"));
    }

    #[test]
    fn allowed_methods_exclude_destructive_rpcs() {
        assert!(!ALLOWED_METHODS.contains(&"sendtoaddress"));
        assert!(!ALLOWED_METHODS.contains(&"sendmany"));
        assert!(!ALLOWED_METHODS.contains(&"sendasset"));
        assert!(!ALLOWED_METHODS.contains(&"fundrawtransaction"));
        assert!(!ALLOWED_METHODS.contains(&"settxfee"));
        assert!(!ALLOWED_METHODS.contains(&"walletcreatefundedpsbt"));
        assert!(!ALLOWED_METHODS.contains(&"signrawtransaction"));
        assert!(!ALLOWED_METHODS.contains(&"issue"));
        assert!(!ALLOWED_METHODS.contains(&"transfer"));
        assert!(!ALLOWED_METHODS.contains(&"walletpassphrase"));
        assert!(!ALLOWED_METHODS.contains(&"encryptwallet"));
        assert!(!ALLOWED_METHODS.contains(&"dumpwallet"));
        assert!(!ALLOWED_METHODS.contains(&"sendmessage"));
        assert!(!ALLOWED_METHODS.contains(&"subscribetochannel"));
        assert!(!ALLOWED_METHODS.contains(&"unsubscribefromchannel"));
        assert!(!ALLOWED_METHODS.contains(&"clearmessages"));
    }

    #[test]
    fn rpc_result_success_constructor() {
        let r = RpcResult {
            success: true,
            data: serde_json::Value::String("ok".into()),
            error: String::new(),
        };
        assert!(r.success);
        assert_eq!(r.data, "ok");
        assert!(r.error.is_empty());
    }

    #[test]
    fn rpc_result_error_constructor() {
        let r = RpcResult {
            success: false,
            data: serde_json::Value::Null,
            error: "something went wrong".to_string(),
        };
        assert!(!r.success);
        assert_eq!(r.data, serde_json::Value::Null);
        assert_eq!(r.error, "something went wrong");
    }

    #[test]
    fn allowed_methods_exclude_mining_rpcs() {
        assert!(!ALLOWED_METHODS.contains(&"getblocktemplate"));
        assert!(!ALLOWED_METHODS.contains(&"getkawpowhash"));
        assert!(!ALLOWED_METHODS.contains(&"submitblock"));
        assert!(!ALLOWED_METHODS.contains(&"pprpcsb"));
        assert!(!ALLOWED_METHODS.contains(&"prioritisetransaction"));
    }

    fn make_temp_dir() -> std::path::PathBuf {
        static CNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = CNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("hemp0x_rpc_test_{:x}_{:x}", std::process::id(), n));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn temp_cookie_path(tmp: &std::path::Path) -> std::path::PathBuf {
        tmp.join(".cookie")
    }

    fn temp_config_path(tmp: &std::path::Path) -> std::path::PathBuf {
        tmp.join("hemp.conf")
    }

    #[test]
    fn auth_userpass_preferred_when_active_static_credentials_exist() {
        let tmp = make_temp_dir();
        let cookie_path = temp_cookie_path(&tmp);
        let config_path = temp_config_path(&tmp);

        fs::write(&cookie_path, "cookie_user:cookie_pass").unwrap();
        fs::write(
            &config_path,
            "rpcuser=config_user\nrpcpassword=config_pass\n",
        )
        .unwrap();

        let result = rpc_auth_for_paths(&cookie_path, &config_path);
        let _ = fs::remove_dir_all(&tmp);

        let (header, mode) = result.unwrap();
        assert_eq!(mode, RpcAuthMode::LegacyUserpass);
        assert!(header.starts_with("Basic "));
    }

    #[test]
    fn auth_falls_back_to_userpass_when_cookie_absent() {
        let tmp = make_temp_dir();
        let cookie_path = temp_cookie_path(&tmp);
        let config_path = temp_config_path(&tmp);

        fs::write(&config_path, "rpcuser=test_user\nrpcpassword=test_pass\n").unwrap();

        let result = rpc_auth_for_paths(&cookie_path, &config_path);
        let _ = fs::remove_dir_all(&tmp);

        let (header, mode) = result.unwrap();
        assert_eq!(mode, RpcAuthMode::LegacyUserpass);
        assert!(header.starts_with("Basic "));
    }

    #[test]
    fn auth_unavailable_when_neither_cookie_nor_userpass() {
        let tmp = make_temp_dir();
        let cookie_path = temp_cookie_path(&tmp);
        let config_path = temp_config_path(&tmp);

        fs::write(&config_path, "server=1\ndaemon=0\n").unwrap();

        let result = rpc_auth_for_paths(&cookie_path, &config_path);
        let _ = fs::remove_dir_all(&tmp);

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unavailable"));
    }

    #[test]
    fn auth_falls_back_on_empty_cookie() {
        let tmp = make_temp_dir();
        let cookie_path = temp_cookie_path(&tmp);
        let config_path = temp_config_path(&tmp);

        fs::write(&cookie_path, "").unwrap();
        fs::write(&config_path, "rpcuser=test_user\nrpcpassword=test_pass\n").unwrap();

        let result = rpc_auth_for_paths(&cookie_path, &config_path);
        let _ = fs::remove_dir_all(&tmp);

        let (_, mode) = result.unwrap();
        assert_eq!(mode, RpcAuthMode::LegacyUserpass);
    }

    #[test]
    fn auth_falls_back_on_malformed_cookie_no_colon() {
        let tmp = make_temp_dir();
        let cookie_path = temp_cookie_path(&tmp);
        let config_path = temp_config_path(&tmp);

        fs::write(&cookie_path, "no_colon_cookie_value").unwrap();
        fs::write(&config_path, "rpcuser=test_user\nrpcpassword=test_pass\n").unwrap();

        let result = rpc_auth_for_paths(&cookie_path, &config_path);
        let _ = fs::remove_dir_all(&tmp);

        let (_, mode) = result.unwrap();
        assert_eq!(mode, RpcAuthMode::LegacyUserpass);
    }

    #[test]
    fn auth_falls_back_on_whitespace_only_cookie() {
        let tmp = make_temp_dir();
        let cookie_path = temp_cookie_path(&tmp);
        let config_path = temp_config_path(&tmp);

        fs::write(&cookie_path, "   \n  ").unwrap();
        fs::write(&config_path, "rpcuser=test_user\nrpcpassword=test_pass\n").unwrap();

        let result = rpc_auth_for_paths(&cookie_path, &config_path);
        let _ = fs::remove_dir_all(&tmp);

        let (_, mode) = result.unwrap();
        assert_eq!(mode, RpcAuthMode::LegacyUserpass);
    }

    #[test]
    fn rpc_auth_mode_serialization() {
        let cookie_mode = serde_json::to_string(&RpcAuthMode::Cookie).unwrap();
        assert_eq!(cookie_mode, "\"cookie\"");
        let legacy_mode = serde_json::to_string(&RpcAuthMode::LegacyUserpass).unwrap();
        assert_eq!(legacy_mode, "\"legacy_userpass\"");
        let unavailable_mode = serde_json::to_string(&RpcAuthMode::Unavailable).unwrap();
        assert_eq!(unavailable_mode, "\"unavailable\"");
    }

    #[test]
    fn rpc_auth_info_warning_for_legacy() {
        let info = RpcAuthInfo {
            auth_mode: RpcAuthMode::LegacyUserpass,
            cookie_exists: false,
            legacy_credentials_exist: true,
            warning: "test warning".to_string(),
        };
        assert!(!info.warning.is_empty());
        assert_eq!(info.auth_mode, RpcAuthMode::LegacyUserpass);
    }

    #[test]
    fn rpc_auth_info_no_warning_for_cookie() {
        let info = RpcAuthInfo {
            auth_mode: RpcAuthMode::Cookie,
            cookie_exists: true,
            legacy_credentials_exist: true,
            warning: String::new(),
        };
        assert!(info.warning.is_empty());
        assert_eq!(info.auth_mode, RpcAuthMode::Cookie);
    }
}
