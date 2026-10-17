// Ravencoin wallet and asset commands for Commander multi-chain support.
//
// Provides send, receive, asset listing/transfer, and transaction history
// for the Ravencoin chain via ravend RPC.
//
// Copyright (c) 2024-2025 The Hemp0x developers
// Distributed under the MIT software license.

use std::time::Duration;

use crate::modules::rpc::RpcResult;
use crate::modules::rvn_models::{
    RvnAssetData, RvnAssetItem, RvnTransactionHistoryItem, RvnTransactionHistoryResult,
};
use crate::modules::rvn_rpc::{call_rvn_rpc, call_rvn_rpc_with_timeouts};

// ─── Address Commands ────────────────────────────────────────────────────────

#[tauri::command]
pub async fn rvn_new_address(label: Option<String>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // Ravencoin getnewaddress accepts [account]. If empty, it's the default account.
        let params: Vec<serde_json::Value> = if let Some(lbl) = label {
            if lbl.is_empty() {
                vec![]
            } else {
                vec![serde_json::Value::String(lbl)]
            }
        } else {
            vec![]
        };
        let result = call_rvn_rpc("getnewaddress", &params)?;
        
        // Sometimes the result is just a string, sometimes it's wrapped.
        if let Some(addr) = result.as_str() {
            return Ok(addr.to_string());
        }
        
        Ok(format!("{}", result).replace("\"", ""))
    })
    .await
    .map_err(|e| format!("RVN new address task failed: {e}"))?
}

#[tauri::command]
pub async fn rvn_get_receive_addresses() -> Result<Vec<serde_json::Value>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut addresses = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // 1. Get all addresses with a balance
        if let Ok(groupings) = call_rvn_rpc("listaddressgroupings", &[]) {
            if let Some(groups) = groupings.as_array() {
                for group in groups {
                    if let Some(addrs) = group.as_array() {
                        for addr_info in addrs {
                            if let Some(info) = addr_info.as_array() {
                                if info.len() >= 2 {
                                    if let Some(addr_str) = info[0].as_str() {
                                        let bal = info[1].as_f64().unwrap_or(0.0);
                                        let label = if info.len() > 2 { info[2].as_str().unwrap_or("").to_string() } else { "".to_string() };
                                        if !seen.contains(addr_str) && label != "__deleted__" {
                                            seen.insert(addr_str.to_string());
                                            addresses.push(serde_json::json!({
                                                "address": addr_str,
                                                "balance": format!("{:.8}", bal),
                                                "label": label,
                                            }));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. Get labeled/received addresses (even if 0 balance)
        if let Ok(result) = call_rvn_rpc("listreceivedbyaddress", &[serde_json::json!(0), serde_json::json!(true)]) {
            if let Some(entries) = result.as_array() {
                for entry in entries {
                    let addr = entry.get("address").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let balance = entry.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let label = entry.get("label")
                        .or_else(|| entry.get("account"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    
                    if !addr.is_empty() && label != "__deleted__" && !seen.contains(&addr) {
                        seen.insert(addr.clone());
                        addresses.push(serde_json::json!({
                            "address": addr,
                            "balance": format!("{:.8}", balance),
                            "label": label,
                        }));
                    }
                }
            }
        }

        // Sort by balance descending
        addresses.sort_by(|a, b| {
            let b_bal = b.get("balance").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
            let a_bal = a.get("balance").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
            b_bal.partial_cmp(&a_bal).unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(addresses)
    })
    .await
    .map_err(|e| format!("RVN receive addresses task failed: {e}"))?
}

#[tauri::command]
pub async fn rvn_delete_receive_address(address: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if address.is_empty() {
            return Err("Address is required.".to_string());
        }
        // Try setaccount first (legacy/Ravencoin format)
        if call_rvn_rpc(
            "setaccount",
            &[
                serde_json::Value::String(address.clone()),
                serde_json::Value::String("__deleted__".to_string()),
            ],
        )
        .is_err()
        {
            // Fallback to setlabel if setaccount doesn't exist
            call_rvn_rpc(
                "setlabel",
                &[
                    serde_json::Value::String(address),
                    serde_json::Value::String("__deleted__".to_string()),
                ],
            )?;
        }
        Ok("Address deleted.".to_string())
    })
    .await
    .map_err(|e| format!("Delete address task failed: {e}"))?
}

// ─── Send Commands ───────────────────────────────────────────────────────────

#[tauri::command]
pub async fn rvn_send_rvn(
    address: String,
    amount: f64,
    comment: Option<String>,
    from_address: Option<String>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dest = address.trim().to_string();
        if dest.is_empty() {
            return Err("Destination address is required.".to_string());
        }
        if !amount.is_finite() || amount <= 0.0 {
            return Err("Amount must be greater than zero.".to_string());
        }

        if let Some(from) = from_address.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
            // Coin-control send from a specific RVN address using UTXO selection
            let utxos_val = call_rvn_rpc(
                "listunspent",
                &[
                    serde_json::json!(0),
                    serde_json::json!(9999999),
                    serde_json::json!([from.clone()]),
                ],
            )?;
            let utxos = utxos_val
                .as_array()
                .ok_or_else(|| "Unexpected response from listunspent".to_string())?;

            let mut selected_inputs = Vec::new();
            let mut total_in_sats: i64 = 0;
            let target_sats = (amount * 100_000_000.0).round() as i64;

            for u in utxos {
                if u.get("spendable").and_then(|v| v.as_bool()) == Some(false)
                    || u.get("assetName").is_some()
                {
                    continue;
                }
                let u_amt = u.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let u_sats = (u_amt * 100_000_000.0).round() as i64;
                if u_sats <= 0 {
                    continue;
                }
                let txid = u.get("txid").and_then(|v| v.as_str()).unwrap_or("");
                let vout = u.get("vout").and_then(|v| v.as_u64()).unwrap_or(0);
                if txid.is_empty() {
                    continue;
                }
                selected_inputs.push(serde_json::json!({
                    "txid": txid,
                    "vout": vout,
                }));
                total_in_sats += u_sats;

                let est_bytes = (148 * selected_inputs.len() + 34 * 2 + 10) as i64;
                let fee_sats = ((est_bytes * 1200) / 1000).max(250_000); // ~0.012 RVN/KB, min 0.0025 RVN
                if total_in_sats >= target_sats + fee_sats {
                    break;
                }
            }

            let est_bytes = (148 * selected_inputs.len().max(1) + 34 * 2 + 10) as i64;
            let fee_sats = ((est_bytes * 1200) / 1000).max(250_000);
            if selected_inputs.is_empty() || total_in_sats < target_sats + fee_sats {
                return Err(format!(
                    "Insufficient RVN balance in address {} (available: {:.8} RVN, required with fee: {:.8} RVN).",
                    from,
                    total_in_sats as f64 / 100_000_000.0,
                    (target_sats + fee_sats) as f64 / 100_000_000.0
                ));
            }

            let change_sats = total_in_sats - target_sats - fee_sats;
            let mut outputs = serde_json::Map::new();
            // Avoid duplicate JSON keys when sending back to the same address
            if dest == from {
                let net_val = ((total_in_sats - fee_sats) as f64 / 100_000_000.0 * 1e8).round() / 1e8;
                outputs.insert(dest, serde_json::json!(net_val));
            } else {
                let send_val = (target_sats as f64 / 100_000_000.0 * 1e8).round() / 1e8;
                outputs.insert(dest, serde_json::json!(send_val));
                // Include change output if above dust threshold (0.0005 RVN = 50,000 sats)
                if change_sats >= 50_000 {
                    let change_val = (change_sats as f64 / 100_000_000.0 * 1e8).round() / 1e8;
                    outputs.insert(from, serde_json::json!(change_val));
                }
            }

            let raw_hex_val = call_rvn_rpc(
                "createrawtransaction",
                &[
                    serde_json::Value::Array(selected_inputs),
                    serde_json::Value::Object(outputs),
                ],
            )?;
            let raw_hex = raw_hex_val
                .as_str()
                .ok_or_else(|| "Unexpected response from createrawtransaction".to_string())?;

            let signed_val = call_rvn_rpc(
                "signrawtransaction",
                &[serde_json::Value::String(raw_hex.to_string())],
            )?;
            let complete = signed_val
                .get("complete")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !complete {
                let err_detail = signed_val
                    .get("errors")
                    .and_then(|e| e.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|first| first.get("error"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("Wallet may be locked (error code: -13) or missing private key.");
                return Err(format!("Failed to sign Ravencoin transaction: {err_detail}"));
            }
            let signed_hex = signed_val
                .get("hex")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing signed transaction hex".to_string())?;

            let sent_val = call_rvn_rpc(
                "sendrawtransaction",
                &[serde_json::Value::String(signed_hex.to_string())],
            )?;
            return sent_val
                .as_str()
                .map(|s| s.to_string())
                .ok_or_else(|| "Unexpected response from sendrawtransaction".to_string());
        }

        let params = vec![
            serde_json::Value::String(dest),
            serde_json::json!(amount),
            serde_json::Value::String(comment.unwrap_or_default()),
            serde_json::Value::String(String::new()),
            serde_json::Value::Bool(false),
        ];

        let result = call_rvn_rpc("sendtoaddress", &params)?;
        result
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| "Unexpected response from sendtoaddress".to_string())
    })
    .await
    .map_err(|e| format!("RVN send task failed: {e}"))?
}

#[tauri::command]
pub async fn rvn_preview_send(
    address: String,
    amount: f64,
    from_address: Option<String>,
) -> RpcResult {
    tauri::async_runtime::spawn_blocking(move || {
        // Validate address
        let addr_check = call_rvn_rpc(
            "validateaddress",
            &[serde_json::Value::String(address.clone())],
        );
        let is_valid = addr_check
            .as_ref()
            .ok()
            .and_then(|v| v["isvalid"].as_bool())
            .unwrap_or(false);

        if !is_valid {
            return RpcResult {
                success: false,
                data: serde_json::Value::Null,
                error: "Invalid Ravencoin address.".to_string(),
            };
        }

        // Get balance (either for specific from_address or whole wallet)
        let balance = if let Some(from) = from_address.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
            call_rvn_rpc(
                "listunspent",
                &[
                    serde_json::json!(0),
                    serde_json::json!(9999999),
                    serde_json::json!([from]),
                ],
            )
            .ok()
            .and_then(|v| v.as_array().cloned())
            .map(|arr| {
                arr.iter()
                    .filter(|u| {
                        u.get("spendable").and_then(|v| v.as_bool()) != Some(false)
                            && u.get("assetName").is_none()
                    })
                    .filter_map(|u| u.get("amount").and_then(|v| v.as_f64()))
                    .sum::<f64>()
            })
            .unwrap_or(0.0)
        } else {
            let wallet_info = call_rvn_rpc("getwalletinfo", &[]);
            wallet_info
                .as_ref()
                .ok()
                .and_then(|v| v["balance"].as_f64())
                .unwrap_or(0.0)
        };

        // Estimate fee
        let fee_result = call_rvn_rpc(
            "estimatesmartfee",
            &[serde_json::json!(6)],
        );
        let fee_rate = fee_result
            .as_ref()
            .ok()
            .and_then(|v| v["feerate"].as_f64())
            .unwrap_or(0.012);
        // Rough estimate: ~250 bytes for a simple tx
        let fee_estimate = (fee_rate * 250.0 / 1000.0).max(0.0025);

        let mut warnings = Vec::new();
        if amount > balance {
            warnings.push("Insufficient balance for this transaction.".to_string());
        } else if amount + fee_estimate > balance {
            warnings.push("Balance may be insufficient after network fees.".to_string());
        }

        let validated = amount > 0.0 && (amount + fee_estimate) <= balance + 1e-8 && is_valid;

        let preview = serde_json::json!({
            "destination": address,
            "amount": format!("{:.8}", amount),
            "asset": "RVN",
            "available_balance": format!("{:.8}", balance),
            "fee_estimate": format!("{:.8}", fee_estimate),
            "warnings": warnings,
            "summary": format!("Send {} RVN to {}", amount, address),
            "validated": validated,
        });

        RpcResult {
            success: true,
            data: preview,
            error: String::new(),
        }
    })
    .await
    .unwrap_or_else(|e| RpcResult {
        success: false,
        data: serde_json::Value::Null,
        error: format!("RVN preview send task failed: {e}"),
    })
}

// ─── Asset Commands ──────────────────────────────────────────────────────────

#[tauri::command]
pub async fn rvn_list_assets() -> Result<Vec<RvnAssetItem>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let result = call_rvn_rpc("listmyassets", &[])?;
        let mut assets = Vec::new();

        if let Some(obj) = result.as_object() {
            for (name, balance) in obj {
                let balance_val = balance.as_f64().unwrap_or(0.0);
                let asset_type = if name.starts_with('#') {
                    "qualifier"
                } else if name.starts_with('$') {
                    "restricted"
                } else if name.contains('#') {
                    "unique"
                } else if name.contains('/') {
                    "sub-asset"
                } else {
                    "root"
                };
                assets.push(RvnAssetItem {
                    name: name.clone(),
                    balance: format!("{:.8}", balance_val),
                    asset_type: asset_type.to_string(),
                });
            }
        }

        // Sort by name
        assets.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(assets)
    })
    .await
    .map_err(|e| format!("RVN list assets task failed: {e}"))?
}

#[tauri::command]
pub async fn rvn_get_asset_data(asset_name: String) -> Result<RvnAssetData, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let result = call_rvn_rpc(
            "getassetdata",
            &[serde_json::Value::String(asset_name)],
        )?;

        Ok(RvnAssetData {
            name: result["name"].as_str().unwrap_or("").to_string(),
            amount: result["amount"].as_f64().unwrap_or(0.0),
            units: result["units"].as_u64().unwrap_or(0) as u8,
            reissuable: result["reissuable"].as_bool().unwrap_or(false),
            has_ipfs: result["has_ipfs"].as_bool().unwrap_or(false),
            ipfs_hash: result["ipfs_hash"].as_str().unwrap_or("").to_string(),
            block_height: result["block_height"].as_u64().unwrap_or(0),
        })
    })
    .await
    .map_err(|e| format!("RVN get asset data task failed: {e}"))?
}

#[tauri::command]
pub async fn rvn_transfer_asset(
    asset_name: String,
    qty: f64,
    to_address: String,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if asset_name.is_empty() {
            return Err("Asset name is required.".to_string());
        }
        if to_address.is_empty() {
            return Err("Destination address is required.".to_string());
        }
        if qty <= 0.0 {
            return Err("Quantity must be greater than zero.".to_string());
        }

        let result = call_rvn_rpc(
            "transfer",
            &[
                serde_json::Value::String(asset_name),
                serde_json::json!(qty),
                serde_json::Value::String(to_address),
            ],
        )?;

        // The result is a txid (array with one string)
        if let Some(arr) = result.as_array() {
            if let Some(txid) = arr.first().and_then(|v| v.as_str()) {
                return Ok(txid.to_string());
            }
        }
        if let Some(txid) = result.as_str() {
            return Ok(txid.to_string());
        }

        Ok(format!("{}", result))
    })
    .await
    .map_err(|e| format!("RVN transfer asset task failed: {e}"))?
}

#[tauri::command]
pub async fn rvn_list_network_assets(
    prefix: Option<String>,
    count: Option<u64>,
    start: Option<u64>,
) -> RpcResult {
    tauri::async_runtime::spawn_blocking(move || {
        let mut params: Vec<serde_json::Value> = Vec::new();
        params.push(serde_json::Value::String(
            prefix.unwrap_or_else(|| "*".to_string()),
        ));
        if count.is_some() || start.is_some() {
            params.push(serde_json::Value::Bool(true)); // verbose
        }

        match call_rvn_rpc("listassets", &params) {
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
    })
    .await
    .unwrap_or_else(|e| RpcResult {
        success: false,
        data: serde_json::Value::Null,
        error: format!("RVN list network assets task failed: {e}"),
    })
}

// ─── Transaction History ─────────────────────────────────────────────────────

#[tauri::command]
pub async fn rvn_get_transaction_history(
    count: Option<u64>,
    skip: Option<u64>,
) -> Result<RvnTransactionHistoryResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let c = count.unwrap_or(50);
        let s = skip.unwrap_or(0);

        let params = [
            serde_json::Value::String("*".to_string()),
            serde_json::json!(c + 1), // Request one extra to detect has_more
            serde_json::json!(s),
        ];
        let result = call_rvn_rpc_with_timeouts(
            "listtransactions",
            &params,
            Duration::from_secs(5),
            Duration::from_secs(30),
        )?;

        let raw_items = result.as_array().cloned().unwrap_or_default();
        let has_more = raw_items.len() > c as usize;
        let total = raw_items.len().min(c as usize);

        use chrono::{DateTime, Local, TimeZone};
        let items: Vec<RvnTransactionHistoryItem> = raw_items
            .iter()
            .take(c as usize)
            .rev()
            .map(|tx| {
                let epoch = tx["time"].as_i64().unwrap_or(0);
                let dt: DateTime<Local> = Local
                    .timestamp_opt(epoch, 0)
                    .single()
                    .unwrap_or_else(|| Local::now());
                let amount = tx["amount"].as_f64().unwrap_or(0.0);

                RvnTransactionHistoryItem {
                    txid: tx["txid"].as_str().unwrap_or("-").to_string(),
                    date: dt.format("%Y-%m-%d %H:%M").to_string(),
                    tx_type: tx["category"].as_str().unwrap_or("unknown").to_string(),
                    amount: format!("{:.8}", amount),
                    confirmations: tx["confirmations"].as_u64().unwrap_or(0),
                    address: tx["address"].as_str().map(|s| s.to_string()),
                    asset: tx["asset"].as_str().map(|s| s.to_string()),
                }
            })
            .collect();

        Ok(RvnTransactionHistoryResult {
            items,
            total,
            has_more,
        })
    })
    .await
    .map_err(|e| format!("RVN transaction history task failed: {e}"))?
}

// ─── UTXO Listing ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn rvn_list_utxos() -> RpcResult {
    tauri::async_runtime::spawn_blocking(|| {
        match call_rvn_rpc_with_timeouts(
            "listunspent",
            &[],
            Duration::from_secs(5),
            Duration::from_secs(30),
        ) {
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
    })
    .await
    .unwrap_or_else(|e| RpcResult {
        success: false,
        data: serde_json::Value::Null,
        error: format!("RVN list UTXOs task failed: {e}"),
    })
}

// ─── Wallet Encryption/Unlock ────────────────────────────────────────────────

#[tauri::command]
pub async fn rvn_wallet_unlock(passphrase: String, timeout: u64) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        call_rvn_rpc(
            "walletpassphrase",
            &[
                serde_json::Value::String(passphrase),
                serde_json::json!(timeout),
            ],
        )?;
        Ok("Ravencoin wallet unlocked.".to_string())
    })
    .await
    .map_err(|e| format!("RVN wallet unlock task failed: {e}"))?
}

#[tauri::command]
pub async fn rvn_wallet_lock() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        call_rvn_rpc("walletlock", &[])?;
        Ok("Ravencoin wallet locked.".to_string())
    })
    .await
    .map_err(|e| format!("RVN wallet lock task failed: {e}"))?
}

#[tauri::command]
pub async fn rvn_wallet_encrypt(passphrase: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        match call_rvn_rpc(
            "encryptwallet",
            &[serde_json::Value::String(passphrase)],
        ) {
            Ok(_) => Ok("Ravencoin wallet encrypted. Daemon will restart.".to_string()),
            Err(e) => {
                if e.contains("transport error") || e.to_lowercase().contains("eof") || e.to_lowercase().contains("closed") {
                    Ok("Ravencoin wallet encrypted. Daemon is restarting.".to_string())
                } else {
                    Err(e)
                }
            }
        }
    })
    .await
    .map_err(|e| format!("RVN wallet encrypt task failed: {e}"))?
}
