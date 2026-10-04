// Atomic Swap engine for Hemp0x ↔ Ravencoin cross-chain trades.
//
// Uses Hash Time-Locked Contracts (HTLCs) that work identically on both
// chains since they share the same Bitcoin-derived script engine. Supports
// both base coin swaps (HEMP ↔ RVN) and asset swaps.
//
// HTLC Script (identical bytecode on both chains):
//   OP_IF
//     OP_SHA256 <secret_hash> OP_EQUALVERIFY
//     OP_DUP OP_HASH160 <recipient_pubkey_hash> OP_EQUALVERIFY OP_CHECKSIG
//   OP_ELSE
//     <locktime> OP_CHECKLOCKTIMEVERIFY OP_DROP
//     OP_DUP OP_HASH160 <sender_pubkey_hash> OP_EQUALVERIFY OP_CHECKSIG
//   OP_ENDIF
//
// Copyright (c) 2024-2025 The Hemp0x developers
// Distributed under the MIT software license.

use std::collections::HashMap;
use std::fs;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use base64::Engine;
use crate::modules::rvn_models::{SwapAcceptance, SwapOffer, SwapStatus, SwapSummary};
use crate::modules::{rpc, rvn_rpc};

// ─── Swap Storage ────────────────────────────────────────────────────────────
// Active swaps are stored in memory and persisted to a JSON file in the
// Commander settings directory.

#[derive(Serialize, Deserialize, Clone, Debug)]
struct SwapRecord {
    offer: SwapOffer,
    acceptance: Option<SwapAcceptance>,
    secret: Option<String>, // hex-encoded secret (only for initiator)
    phase: String,
    updated_at: u64,
}

fn swap_store() -> &'static Mutex<HashMap<String, SwapRecord>> {
    static STORE: OnceLock<Mutex<HashMap<String, SwapRecord>>> = OnceLock::new();
    STORE.get_or_init(|| {
        let store = load_swap_store().unwrap_or_default();
        Mutex::new(store)
    })
}

fn swap_store_path() -> Result<std::path::PathBuf, String> {
    let base = dirs::config_dir()
        .or_else(|| dirs::data_dir())
        .ok_or("Cannot determine config directory")?;
    let dir = base.join("hemp0x-commander");
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| format!("Failed to create swap store dir: {e}"))?;
    }
    Ok(dir.join("atomic_swaps.json"))
}

fn load_swap_store() -> Result<HashMap<String, SwapRecord>, String> {
    let path = swap_store_path()?;
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read swap store: {e}"))?;
    serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse swap store: {e}"))
}

fn persist_swap_store(store: &HashMap<String, SwapRecord>) -> Result<(), String> {
    let path = swap_store_path()?;
    let content = serde_json::to_string_pretty(store)
        .map_err(|e| format!("Failed to serialize swap store: {e}"))?;
    fs::write(&path, content)
        .map_err(|e| format!("Failed to write swap store: {e}"))
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ─── HTLC Script Construction ────────────────────────────────────────────────
// Both Hemp0x and Ravencoin use the same opcodes. These functions build the
// redeem script for the HTLC.

/// Generate a cryptographically secure 32-byte secret.
fn generate_secret() -> Vec<u8> {
    use rand::RngCore;
    let mut secret = vec![0u8; 32];
    rand::thread_rng().fill_bytes(&mut secret);
    secret
}

/// SHA256 hash of the secret.
fn sha256_hash(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

/// Extract the 20-byte pubkey hash from a base58check P2PKH address.
fn extract_pubkey_hash(address: &str) -> Result<Vec<u8>, String> {
    let decoded = bs58::decode(address)
        .into_vec()
        .map_err(|e| format!("Invalid base58 address: {e}"))?;
    if decoded.len() != 25 {
        return Err("Address decoded to incorrect length (expected 25 bytes)".to_string());
    }
    Ok(decoded[1..21].to_vec())
}

/// OP_IF
///   OP_SHA256 <secret_hash_32> OP_EQUALVERIFY
/// OP_ELSE
///   <locktime_le_bytes> OP_CHECKLOCKTIMEVERIFY OP_DROP
///   OP_DUP OP_HASH160 <sender_pubkey_hash_20> OP_EQUALVERIFY OP_CHECKSIG
/// OP_ENDIF
fn build_htlc_script(
    secret_hash: &[u8],
    sender_pubkey_hash: &[u8],
    recipient_pubkey_hash: &[u8],
    locktime: u64,
) -> Vec<u8> {
    let mut script = Vec::new();

    // OP_IF
    script.push(0x63);

    // OP_SHA256
    script.push(0xa8);
    // Push 32 bytes (secret hash)
    script.push(0x20);
    script.extend_from_slice(secret_hash);
    // OP_EQUALVERIFY
    script.push(0x88);

    // OP_DUP OP_HASH160 <recipient_pubkey_hash> OP_EQUALVERIFY OP_CHECKSIG
    script.push(0x76);
    script.push(0xa9);
    script.push(0x14);
    script.extend_from_slice(recipient_pubkey_hash);
    script.push(0x88);
    script.push(0xac);

    // OP_ELSE
    script.push(0x67);

    // Push locktime as minimal CScriptNum
    let locktime_bytes = encode_script_num(locktime as i64);
    if locktime_bytes.len() == 1 {
        script.push(locktime_bytes[0]);
    } else {
        script.push(locktime_bytes.len() as u8);
        script.extend_from_slice(&locktime_bytes);
    }

    // OP_CHECKLOCKTIMEVERIFY
    script.push(0xb1);
    // OP_DROP
    script.push(0x75);

    // OP_DUP
    script.push(0x76);
    // OP_HASH160
    script.push(0xa9);
    // Push 20 bytes (sender pubkey hash)
    script.push(0x14);
    script.extend_from_slice(sender_pubkey_hash);
    // OP_EQUALVERIFY
    script.push(0x88);
    // OP_CHECKSIG
    script.push(0xac);

    // OP_ENDIF
    script.push(0x68);

    script
}

/// Legacy HTLC script (without recipient check) for refunding older swaps.
fn build_legacy_htlc_script(
    secret_hash: &[u8],
    sender_pubkey_hash: &[u8],
    locktime: u64,
) -> Vec<u8> {
    let mut script = Vec::new();
    script.push(0x63); // OP_IF
    script.push(0xa8); // OP_SHA256
    script.push(0x20); // Push 32 bytes
    script.extend_from_slice(secret_hash);
    script.push(0x88); // OP_EQUALVERIFY
    script.push(0x67); // OP_ELSE
    let locktime_bytes = encode_script_num(locktime as i64);
    if locktime_bytes.len() == 1 {
        script.push(locktime_bytes[0]);
    } else {
        script.push(locktime_bytes.len() as u8);
        script.extend_from_slice(&locktime_bytes);
    }
    script.push(0xb1); // OP_CHECKLOCKTIMEVERIFY
    script.push(0x75); // OP_DROP
    script.push(0x76); // OP_DUP
    script.push(0xa9); // OP_HASH160
    script.push(0x14); // Push 20 bytes
    script.extend_from_slice(sender_pubkey_hash);
    script.push(0x88); // OP_EQUALVERIFY
    script.push(0xac); // OP_CHECKSIG
    script.push(0x68); // OP_ENDIF
    script
}

fn push_data(sig: &mut Vec<u8>, data: &[u8]) {
    let len = data.len();
    if len <= 75 {
        sig.push(len as u8);
    } else if len <= 255 {
        sig.push(0x4c); // OP_PUSHDATA1
        sig.push(len as u8);
    } else if len <= 65535 {
        sig.push(0x4d); // OP_PUSHDATA2
        sig.extend_from_slice(&(len as u16).to_le_bytes());
    } else {
        sig.push(0x4e); // OP_PUSHDATA4
        sig.extend_from_slice(&(len as u32).to_le_bytes());
    }
    sig.extend_from_slice(data);
}

/// Helper to build a scriptSig pushing data.
/// For OP_IF claim: `<signature> <pubkey> <secret> OP_TRUE <redeemScript>`
fn build_claim_script_sig(signature: &[u8], pubkey: &[u8], secret: &[u8], redeem_script: &[u8]) -> Vec<u8> {
    let mut sig = Vec::new();
    push_data(&mut sig, signature);
    push_data(&mut sig, pubkey);
    push_data(&mut sig, secret);
    sig.push(0x51); // OP_TRUE
    push_data(&mut sig, redeem_script);
    sig
}

fn build_refund_script_sig(signature: &[u8], pubkey: &[u8], redeem_script: &[u8]) -> Vec<u8> {
    let mut sig = Vec::new();
    push_data(&mut sig, signature);
    push_data(&mut sig, pubkey);
    sig.push(0x00); // OP_FALSE
    push_data(&mut sig, redeem_script);
    sig
}

/// Helper to inject scriptSig into a raw hex tx created by `createrawtransaction` with 1 input.
fn inject_script_sig(raw_tx_hex: &str, script_sig: &[u8]) -> Result<String, String> {
    // The raw tx hex looks like:
    // 02000000 01 <32_byte_txid> <4_byte_vout> 00 <4_byte_sequence> <output_count> ...
    // We need to replace the `00` (empty scriptSig) with our scriptSig.
    // The scriptSig length is a varint.
    
    // Find the input count '01'
    if !raw_tx_hex.starts_with("0200000001") && !raw_tx_hex.starts_with("0100000001") {
        return Err("Unsupported raw tx format (must have version 1 or 2 and exactly 1 input)".to_string());
    }

    // Version (4 bytes) + Input Count (1 byte) = 5 bytes = 10 hex chars
    // TXID (32 bytes) = 64 hex chars
    // VOUT (4 bytes) = 8 hex chars
    // Total prefix = 10 + 64 + 8 = 82 hex chars
    if raw_tx_hex.len() < 82 + 2 {
        return Err("Raw tx hex too short".to_string());
    }

    let prefix = &raw_tx_hex[..82];
    let empty_script_sig_len = &raw_tx_hex[82..84];
    let suffix = &raw_tx_hex[84..];

    if empty_script_sig_len != "00" {
        return Err("Expected empty scriptSig (00) in raw tx".to_string());
    }

    // Build varint for script_sig length
    let len = script_sig.len();
    let varint = if len < 0xfd {
        format!("{:02x}", len)
    } else if len <= 0xffff {
        format!("fd{:02x}{:02x}", len & 0xff, len >> 8)
    } else {
        return Err("scriptSig too large".to_string());
    };

    let script_sig_hex = hex::encode(script_sig);

    Ok(format!("{}{}{}{}", prefix, varint, script_sig_hex, suffix))
}

/// Encode an integer as a Bitcoin CScriptNum (minimal encoding).
fn encode_script_num(value: i64) -> Vec<u8> {
    if value == 0 {
        return vec![0x00];
    }

    let neg = value < 0;
    let mut abs_val = if neg { -value } else { value } as u64;
    let mut result = Vec::new();

    while abs_val > 0 {
        result.push((abs_val & 0xff) as u8);
        abs_val >>= 8;
    }

    if result.last().map(|b| b & 0x80 != 0).unwrap_or(false) {
        result.push(if neg { 0x80 } else { 0x00 });
    } else if neg {
        let last = result.len() - 1;
        result[last] |= 0x80;
    }

    result
}

// ─── Custom SIGHASH & ECDSA Signer ───────────────────────────────────────────

fn read_varint(data: &[u8]) -> Result<(u64, usize), String> {
    if data.is_empty() { return Err("Unexpected EOF".into()); }
    match data[0] {
        0..=252 => Ok((data[0] as u64, 1)),
        253 => {
            if data.len() < 3 { return Err("EOF".into()); }
            Ok((u16::from_le_bytes([data[1], data[2]]) as u64, 3))
        },
        254 => {
            if data.len() < 5 { return Err("EOF".into()); }
            Ok((u32::from_le_bytes(data[1..5].try_into().unwrap()) as u64, 5))
        },
        255 => {
            if data.len() < 9 { return Err("EOF".into()); }
            Ok((u64::from_le_bytes(data[1..9].try_into().unwrap()), 9))
        }
    }
}

fn encode_varint(value: u64) -> Vec<u8> {
    if value < 253 {
        vec![value as u8]
    } else if value <= 0xffff {
        let mut v = vec![253];
        v.extend_from_slice(&(value as u16).to_le_bytes());
        v
    } else if value <= 0xffffffff {
        let mut v = vec![254];
        v.extend_from_slice(&(value as u32).to_le_bytes());
        v
    } else {
        let mut v = vec![255];
        v.extend_from_slice(&value.to_le_bytes());
        v
    }
}

fn build_sighash_all(raw_tx: &[u8], redeem_script: &[u8]) -> Result<Vec<u8>, String> {
    let mut offset = 0;
    
    if raw_tx.len() < 4 { return Err("Tx too short".into()); }
    offset += 4;
    
    let (in_count, in_count_len) = read_varint(&raw_tx[offset..])?;
    if in_count != 1 { return Err("Expected exactly 1 input".into()); }
    offset += in_count_len;
    
    if raw_tx.len() < offset + 36 { return Err("Tx too short".into()); }
    offset += 36;
    
    let (script_len, script_len_len) = read_varint(&raw_tx[offset..])?;
    if script_len != 0 { return Err("Expected empty scriptSig".into()); }
    
    let mut sighash_tx = Vec::new();
    sighash_tx.extend_from_slice(&raw_tx[0..offset]);
    sighash_tx.extend_from_slice(&encode_varint(redeem_script.len() as u64));
    sighash_tx.extend_from_slice(redeem_script);
    
    offset += script_len_len;
    sighash_tx.extend_from_slice(&raw_tx[offset..]);
    
    sighash_tx.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
    
    let hash1 = sha2::Sha256::digest(&sighash_tx);
    let sighash = sha2::Sha256::digest(&hash1);
    Ok(sighash.to_vec())
}

fn sign_sighash(privkey_wif: &str, sighash: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let wif_bytes = bs58::decode(privkey_wif).into_vec().map_err(|e| format!("Invalid WIF: {e}"))?;
    
    let is_compressed = wif_bytes.len() == 38;
    let secret = if is_compressed {
        &wif_bytes[1..33]
    } else if wif_bytes.len() == 37 {
        &wif_bytes[1..33]
    } else {
        return Err(format!("Invalid WIF length: {}", wif_bytes.len()));
    };
    
    let signing_key = k256::ecdsa::SigningKey::from_slice(secret).map_err(|e| format!("Invalid secret key: {e}"))?;
    let verifying_key = signing_key.verifying_key();
    let pubkey_bytes = verifying_key.to_encoded_point(is_compressed).as_bytes().to_vec();
    
    let (signature, _) = signing_key.sign_prehash_recoverable(sighash).map_err(|e| format!("Sign error: {e}"))?;
    let standard_sig: k256::ecdsa::Signature = signature.into();
    let normalized = standard_sig.normalize_s().unwrap_or(standard_sig);
    
    let mut final_sig = normalized.to_der().as_bytes().to_vec();
    final_sig.push(0x01); // SIGHASH_ALL
    
    Ok((final_sig, pubkey_bytes))
}

// ─── RPC Helpers ─────────────────────────────────────────────────────────────

fn rvn_getnewaddress() -> Result<String, String> {
    let res = rvn_rpc::call_rvn_rpc("getnewaddress", &[])?;
    res.as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "Failed to parse RVN getnewaddress".to_string())
}

fn hemp_getnewaddress() -> Result<String, String> {
    let res = rpc::call_active_wallet_or_default("getnewaddress", &[])?;
    res.as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "Failed to parse Hemp getnewaddress".to_string())
}

fn rvn_decodescript(hex_script: &str) -> Result<String, String> {
    let res = rvn_rpc::call_rvn_rpc("decodescript", &[serde_json::Value::String(hex_script.to_string())])?;
    res.get("p2sh")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "No P2SH address in RVN decodescript".to_string())
}

fn hemp_decodescript(hex_script: &str) -> Result<String, String> {
    let res = rpc::call_rpc("decodescript", &[serde_json::Value::String(hex_script.to_string())])?;
    res.get("p2sh")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "No P2SH address in Hemp decodescript".to_string())
}

fn rvn_sendtoaddress(address: &str, amount: f64) -> Result<String, String> {
    let res = rvn_rpc::call_rvn_rpc(
        "sendtoaddress",
        &[serde_json::Value::String(address.to_string()), serde_json::json!(amount)],
    )?;
    res.as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "Failed to parse RVN sendtoaddress txid".to_string())
}

fn hemp_sendtoaddress(address: &str, amount: f64) -> Result<String, String> {
    let res = rpc::call_active_wallet_or_default(
        "sendtoaddress",
        &[serde_json::Value::String(address.to_string()), serde_json::json!(amount)],
    )?;
    res.as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "Failed to parse Hemp sendtoaddress txid".to_string())
}

fn rvn_find_utxo(txid: &str, address: &str) -> Result<(u32, f64, String), String> {
    let res = rvn_rpc::call_rvn_rpc("getrawtransaction", &[serde_json::Value::String(txid.to_string()), serde_json::json!(true)])?;
    let vouts = res.get("vout").and_then(|v| v.as_array()).ok_or("No vout in tx")?;
    for vout in vouts {
        let n = vout.get("n").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let value = vout.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let spk_obj = vout.get("scriptPubKey").ok_or("No scriptPubKey")?;
        let spk_hex = spk_obj.get("hex").and_then(|h| h.as_str()).unwrap_or("").to_string();
        let addrs = spk_obj.get("addresses").and_then(|a| a.as_array());
        if let Some(addrs) = addrs {
            for a in addrs {
                if a.as_str() == Some(address) {
                    return Ok((n, value, spk_hex));
                }
            }
        }
    }
    Err("UTXO not found".to_string())
}

fn hemp_find_utxo(txid: &str, address: &str) -> Result<(u32, f64, String), String> {
    let res = rpc::call_rpc("getrawtransaction", &[serde_json::Value::String(txid.to_string()), serde_json::json!(true)])?;
    let vouts = res.get("vout").and_then(|v| v.as_array()).ok_or("No vout in tx")?;
    for vout in vouts {
        let n = vout.get("n").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let value = vout.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let spk_obj = vout.get("scriptPubKey").ok_or("No scriptPubKey")?;
        let spk_hex = spk_obj.get("hex").and_then(|h| h.as_str()).unwrap_or("").to_string();
        let addrs = spk_obj.get("addresses").and_then(|a| a.as_array());
        if let Some(addrs) = addrs {
            for a in addrs {
                if a.as_str() == Some(address) {
                    return Ok((n, value, spk_hex));
                }
            }
        }
    }
    Err("UTXO not found".to_string())
}

fn rvn_createrawtransaction(inputs: serde_json::Value, outputs: serde_json::Value) -> Result<String, String> {
    let res = rvn_rpc::call_rvn_rpc("createrawtransaction", &[inputs, outputs])?;
    res.as_str().map(|s| s.to_string()).ok_or_else(|| "Failed to parse RVN createrawtransaction".to_string())
}

fn hemp_createrawtransaction(inputs: serde_json::Value, outputs: serde_json::Value) -> Result<String, String> {
    let res = rpc::call_rpc("createrawtransaction", &[inputs, outputs])?;
    res.as_str().map(|s| s.to_string()).ok_or_else(|| "Failed to parse Hemp createrawtransaction".to_string())
}

fn rvn_sendrawtransaction(hex: &str) -> Result<String, String> {
    let res = rvn_rpc::call_rvn_rpc("sendrawtransaction", &[serde_json::Value::String(hex.to_string())])?;
    res.as_str().map(|s| s.to_string()).ok_or_else(|| "Failed to parse RVN sendrawtransaction".to_string())
}

fn hemp_sendrawtransaction(hex: &str) -> Result<String, String> {
    let res = rpc::call_rpc("sendrawtransaction", &[serde_json::Value::String(hex.to_string())])?;
    res.as_str().map(|s| s.to_string()).ok_or_else(|| "Failed to parse Hemp sendrawtransaction".to_string())
}

fn rvn_signrawtransaction(hex: &str, prevtxs: serde_json::Value) -> Result<String, String> {
    let res = rvn_rpc::call_rvn_rpc("signrawtransaction", &[serde_json::Value::String(hex.to_string()), prevtxs])?;
    if !res.get("complete").and_then(|c| c.as_bool()).unwrap_or(false) {
        return Err("RVN signrawtransaction incomplete".to_string());
    }
    res.get("hex").and_then(|h| h.as_str()).map(|s| s.to_string()).ok_or_else(|| "Failed to parse signed hex".to_string())
}

fn hemp_signrawtransaction(hex: &str, prevtxs: serde_json::Value) -> Result<String, String> {
    let res = rpc::call_active_wallet_or_default("signrawtransaction", &[serde_json::Value::String(hex.to_string()), prevtxs])?;
    if !res.get("complete").and_then(|c| c.as_bool()).unwrap_or(false) {
        return Err("Hemp signrawtransaction incomplete".to_string());
    }
    res.get("hex").and_then(|h| h.as_str()).map(|s| s.to_string()).ok_or_else(|| "Failed to parse signed hex".to_string())
}

// ─── Tauri Commands ──────────────────────────────────────────────────────────

/// Generate an intent to trade (Phase 3). No funds are locked yet.
#[tauri::command]
pub async fn swap_generate_intent(
    offer_chain: String,
    offer_amount: f64,
    offer_asset: Option<String>,
    offer_address: String, // Refund address
    want_chain: String,
    want_amount: f64,
    want_asset: Option<String>,
    want_address: String, // Receive address
) -> Result<SwapOffer, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let offer_chain_lower = offer_chain.to_lowercase();
        let want_chain_lower = want_chain.to_lowercase();
        
        let secret = generate_secret();
        let secret_hash = sha256_hash(&secret);
        let secret_hex = hex::encode(&secret);
        let secret_hash_hex = hex::encode(&secret_hash);
        let swap_id = uuid::Uuid::new_v4().to_string();

        let offer_data = serde_json::json!({
            "swap_id": swap_id,
            "secret_hash": secret_hash_hex,
            "offer_chain": offer_chain_lower,
            "offer_amount": offer_amount,
            "offer_asset": offer_asset,
            "offer_address": offer_address,
            "want_chain": want_chain_lower,
            "want_amount": want_amount,
            "want_asset": want_asset,
            "want_address": want_address,
            "htlc_address": "",
            "htlc_txid": "",
            "locktime": 0,
            "version": 3,
        });
        
        let offer_code = base64::prelude::BASE64_STANDARD.encode(offer_data.to_string().as_bytes());

        let offer = SwapOffer {
            swap_id: swap_id.clone(),
            offer_code,
            secret_hash: secret_hash_hex,
            offer_chain: offer_chain_lower,
            offer_amount,
            offer_asset,
            offer_address,
            want_chain: want_chain_lower,
            want_amount,
            want_asset,
            want_address,
            htlc_address: String::new(),
            htlc_txid: String::new(),
            locktime: 0,
            created_at: now_epoch(),
            status: "intent".to_string(),
        };

        let record = SwapRecord {
            offer: offer.clone(),
            secret: Some(secret_hex),
            acceptance: None,
            phase: "intent".to_string(),
            updated_at: now_epoch(),
        };

        let mut store = swap_store().lock().map_err(|_| "Swap store lock poisoned")?;
        store.insert(swap_id, record);
        persist_swap_store(&store)?;

        Ok(offer)
    })
    .await
    .map_err(|e| format!("Swap generate intent task failed: {e}"))?
}

/// Initiator locks funds after receiving Acceptor's handshake.
#[tauri::command]
pub async fn swap_lock_initiator(swap_id: String, acceptor_want_address: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut store = swap_store().lock().map_err(|_| "Swap store lock poisoned")?;
        let record = store.get_mut(&swap_id).ok_or_else(|| format!("Swap not found: {swap_id}"))?;

        if record.phase != "intent" {
            return Err("Swap is not in intent phase".to_string());
        }

        let locktime = now_epoch() + (48 * 60 * 60);

        // Build HTLC using acceptor's pubkey hash
        let recipient_pubkey_hash = extract_pubkey_hash(&acceptor_want_address)?;
        let sender_pubkey_hash = extract_pubkey_hash(&record.offer.offer_address)?;
        let secret_hash_bytes = hex::decode(&record.offer.secret_hash)
            .map_err(|e| format!("Invalid secret hash: {e}"))?;
            
        let htlc_script = build_htlc_script(&secret_hash_bytes, &sender_pubkey_hash, &recipient_pubkey_hash, locktime);
        let htlc_script_hex = hex::encode(&htlc_script);

        let (htlc_address, htlc_txid) = if record.offer.offer_chain == "hemp" {
            let p2sh = hemp_decodescript(&htlc_script_hex)?;
            let txid = hemp_sendtoaddress(&p2sh, record.offer.offer_amount)?;
            (p2sh, txid)
        } else {
            let p2sh = rvn_decodescript(&htlc_script_hex)?;
            let txid = rvn_sendtoaddress(&p2sh, record.offer.offer_amount)?;
            (p2sh, txid)
        };

        record.offer.want_address = acceptor_want_address;
        record.offer.htlc_address = htlc_address;
        record.offer.htlc_txid = htlc_txid;
        record.offer.locktime = locktime;
        record.phase = "offered".to_string();
        record.updated_at = now_epoch();
        
        let offer_json = serde_json::to_string(&record.offer).map_err(|e| e.to_string())?;
        record.offer.offer_code = base64::prelude::BASE64_STANDARD.encode(offer_json.as_bytes());

        let res = record.offer.offer_code.clone();
        persist_swap_store(&store)?;

        Ok(res)
    })
    .await
    .map_err(|e| format!("Swap lock initiator task failed: {e}"))?
}

/// Create a new swap offer (Legacy - Pre-funded HTLC)
#[tauri::command]
pub async fn swap_create_offer(
    offer_chain: String,
    offer_amount: f64,
    offer_asset: Option<String>,
    offer_address: String,
    want_chain: String,
    want_amount: f64,
    want_asset: Option<String>,
    want_address: String,
    locktime_hours: Option<u32>,
) -> Result<SwapOffer, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // Validate chains
        let offer_chain_lower = offer_chain.to_lowercase();
        let want_chain_lower = want_chain.to_lowercase();
        if !["hemp", "rvn"].contains(&offer_chain_lower.as_str()) {
            return Err("offer_chain must be 'hemp' or 'rvn'".to_string());
        }
        if !["hemp", "rvn"].contains(&want_chain_lower.as_str()) {
            return Err("want_chain must be 'hemp' or 'rvn'".to_string());
        }
        if offer_chain_lower == want_chain_lower {
            return Err("offer_chain and want_chain must be different".to_string());
        }
        if offer_amount <= 0.0 || want_amount <= 0.0 {
            return Err("Amounts must be positive".to_string());
        }

        // Generate secret and hash
        let secret = generate_secret();
        let secret_hash = sha256_hash(&secret);
        let secret_hex = hex::encode(&secret);
        let secret_hash_hex = hex::encode(&secret_hash);

        // Calculate locktime (initiator gets longer locktime for safety)
        let hours = locktime_hours.unwrap_or(24) as u64;
        let locktime = now_epoch() + (hours * 3600);

        // Build HTLC script
        let sender_pubkey_hash = extract_pubkey_hash(&offer_address)?;
        let recipient_pubkey_hash = extract_pubkey_hash(&want_address)?;
        let htlc_script = build_htlc_script(&secret_hash, &sender_pubkey_hash, &recipient_pubkey_hash, locktime);
        let htlc_script_hex = hex::encode(&htlc_script);

        // Derive P2SH and fund it
        let (htlc_address, htlc_txid) = if offer_chain_lower == "hemp" {
            let p2sh = hemp_decodescript(&htlc_script_hex)?;
            let txid = hemp_sendtoaddress(&p2sh, offer_amount)?;
            (p2sh, txid)
        } else {
            let p2sh = rvn_decodescript(&htlc_script_hex)?;
            let txid = rvn_sendtoaddress(&p2sh, offer_amount)?;
            (p2sh, txid)
        };

        // Generate swap ID
        let swap_id = uuid::Uuid::new_v4().to_string();

        // Build the offer code (base64-encoded JSON for sharing)
        let offer_data = serde_json::json!({
            "swap_id": swap_id,
            "secret_hash": secret_hash_hex,
            "offer_chain": offer_chain_lower,
            "offer_amount": offer_amount,
            "offer_asset": offer_asset,
            "offer_address": offer_address,
            "want_chain": want_chain_lower,
            "want_amount": want_amount,
            "want_asset": want_asset,
            "want_address": want_address,
            "htlc_address": htlc_address,
            "htlc_txid": htlc_txid,
            "locktime": locktime,
            "version": 1,
        });
        let offer_code = base64::prelude::BASE64_STANDARD
            .encode(offer_data.to_string().as_bytes());

        let offer = SwapOffer {
            swap_id: swap_id.clone(),
            offer_code,
            secret_hash: secret_hash_hex,
            offer_chain: offer_chain_lower,
            offer_amount,
            offer_asset,
            offer_address,
            want_chain: want_chain_lower,
            want_amount,
            want_asset,
            want_address,
            htlc_address,
            htlc_txid,
            locktime,
            created_at: now_epoch(),
            status: "created".to_string(),
        };

        // Store the swap
        let record = SwapRecord {
            offer: offer.clone(),
            acceptance: None,
            secret: Some(secret_hex),
            phase: "created".to_string(),
            updated_at: now_epoch(),
        };

        let mut store = swap_store().lock().map_err(|_| "Swap store lock poisoned")?;
        store.insert(swap_id, record);
        persist_swap_store(&store)?;

        Ok(offer)
    })
    .await
    .map_err(|e| format!("Swap create offer task failed: {e}"))?
}

/// Accept a swap offer from a counterparty.
///
/// The acceptor decodes the offer code, verifies the HTLC on the offering
/// chain, and creates a matching HTLC on the wanted chain with a shorter
/// locktime.
#[tauri::command]
pub async fn swap_accept_offer(offer_code: String, want_address: String, offer_address: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // Decode offer
        let decoded = base64::prelude::BASE64_STANDARD
            .decode(offer_code.as_bytes())
            .map_err(|e| format!("Invalid offer code: {e}"))?;
        let offer_data: serde_json::Value = serde_json::from_slice(&decoded)
            .map_err(|e| format!("Invalid offer JSON: {e}"))?;

        let swap_id = offer_data["swap_id"]
            .as_str()
            .ok_or("Missing swap_id in offer")?
            .to_string();
        let secret_hash = offer_data["secret_hash"]
            .as_str()
            .ok_or("Missing secret_hash in offer")?
            .to_string();
        let offer_chain = offer_data["offer_chain"]
            .as_str()
            .ok_or("Missing offer_chain")?
            .to_string();
        let want_chain = offer_data["want_chain"]
            .as_str()
            .ok_or("Missing want_chain")?
            .to_string();
        let offer_amount = offer_data["offer_amount"]
            .as_f64()
            .ok_or("Missing offer_amount")?;
        let want_amount = offer_data["want_amount"]
            .as_f64()
            .ok_or("Missing want_amount")?;
        let offer_locktime = offer_data["locktime"]
            .as_u64()
            .ok_or("Missing locktime")?;

        let offer_asset = offer_data["offer_asset"].as_str().map(|s| s.to_string());
        let initiator_offer_address = offer_data["offer_address"].as_str().unwrap_or("").to_string();
        let want_asset = offer_data["want_asset"].as_str().map(|s| s.to_string());
        let original_want_address = offer_data["want_address"].as_str().unwrap_or("").to_string();
        let initiator_htlc_address = offer_data["htlc_address"].as_str().unwrap_or("").to_string();
        let initiator_htlc_txid = offer_data["htlc_txid"].as_str().unwrap_or("").to_string();

        // Acceptor uses half the initiator's locktime for safety
        let accept_locktime = now_epoch() + ((offer_locktime - now_epoch()) / 2);

        // Build HTLC script for the chain the acceptor is offering (which is the initiator's want_chain)
        let sender_pubkey_hash = extract_pubkey_hash(&offer_address)?;
        let recipient_pubkey_hash = extract_pubkey_hash(&original_want_address)?;
        let secret_hash_bytes = hex::decode(&secret_hash)
            .map_err(|e| format!("Invalid secret hash in offer: {e}"))?;
        let htlc_script = build_htlc_script(&secret_hash_bytes, &sender_pubkey_hash, &recipient_pubkey_hash, accept_locktime);
        let htlc_script_hex = hex::encode(&htlc_script);

        // Derive P2SH and fund it
        let (accept_htlc_address, accept_htlc_txid) = if want_chain == "hemp" {
            let p2sh = hemp_decodescript(&htlc_script_hex)?;
            let txid = hemp_sendtoaddress(&p2sh, want_amount)?;
            (p2sh, txid)
        } else {
            let p2sh = rvn_decodescript(&htlc_script_hex)?;
            let txid = rvn_sendtoaddress(&p2sh, want_amount)?;
            (p2sh, txid)
        };

        let acceptance = SwapAcceptance {
            swap_id: swap_id.clone(),
            accept_htlc_address: accept_htlc_address.clone(),
            accept_htlc_txid: accept_htlc_txid.clone(),
            accept_locktime,
            want_address: want_address.clone(),
            status: "accepted".to_string(),
            acceptor_refund_address: Some(offer_address.clone()),
        };

        // Store the accepted swap
        let offer = SwapOffer {
            swap_id: swap_id.clone(),
            offer_code: String::new(),
            secret_hash,
            offer_chain,
            offer_amount,
            offer_asset,
            offer_address: initiator_offer_address,
            want_chain,
            want_amount,
            want_asset,
            want_address: original_want_address,
            htlc_address: initiator_htlc_address,
            htlc_txid: initiator_htlc_txid,
            locktime: offer_locktime,
            created_at: now_epoch(),
            status: "accepted".to_string(),
        };

        let record = SwapRecord {
            offer,
            acceptance: Some(acceptance.clone()),
            secret: None, // Acceptor doesn't know the secret yet
            phase: "accepted".to_string(),
            updated_at: now_epoch(),
        };

        let mut store = swap_store().lock().map_err(|_| "Swap store lock poisoned")?;
        store.insert(swap_id.clone(), record);
        persist_swap_store(&store)?;

        // Build the accept code (base64-encoded JSON for sharing)
        let accept_data = serde_json::json!({
            "swap_id": swap_id,
            "accept_htlc_address": accept_htlc_address,
            "accept_htlc_txid": accept_htlc_txid,
            "accept_offer_address": offer_address, // The acceptor's refund address (used as sender_pubkey_hash for their HTLC)
        });
        let accept_code = base64::prelude::BASE64_STANDARD
            .encode(accept_data.to_string().as_bytes());

        Ok(accept_code)
    })
    .await
    .map_err(|e| format!("Swap accept offer task failed: {e}"))?
}

/// Finalize a swap offer (initiator side).
///
/// The initiator receives the accept code, verifies the acceptor's HTLC, and saves the details.
#[tauri::command]
pub async fn swap_finalize_offer(swap_id: String, accept_code: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let decoded = base64::prelude::BASE64_STANDARD
            .decode(accept_code.as_bytes())
            .map_err(|e| format!("Invalid accept code: {e}"))?;
        let accept_data: serde_json::Value = serde_json::from_slice(&decoded)
            .map_err(|e| format!("Invalid accept JSON: {e}"))?;

        let code_swap_id = accept_data["swap_id"]
            .as_str()
            .ok_or("Missing swap_id in accept code")?
            .to_string();
            
        if code_swap_id != swap_id {
            return Err("Accept code is for a different swap".to_string());
        }

        let accept_htlc_address = accept_data["accept_htlc_address"]
            .as_str()
            .ok_or("Missing accept_htlc_address")?
            .to_string();
        let accept_htlc_txid = accept_data["accept_htlc_txid"]
            .as_str()
            .ok_or("Missing accept_htlc_txid")?
            .to_string();
        let accept_offer_address = accept_data["accept_offer_address"]
            .as_str()
            .ok_or("Missing accept_offer_address")?
            .to_string();

        let mut store = swap_store().lock().map_err(|_| "Swap store lock poisoned")?;
        let record = store
            .get_mut(&swap_id)
            .ok_or_else(|| format!("Swap not found: {swap_id}"))?;

        if record.phase != "created" {
            return Err("Swap is not in created phase".to_string());
        }

        let offer_locktime = record.offer.locktime;
        let accept_locktime = now_epoch() + ((offer_locktime - now_epoch()) / 2);

        let acceptance = SwapAcceptance {
            swap_id: swap_id.clone(),
            accept_htlc_address,
            accept_htlc_txid,
            accept_locktime,
            want_address: accept_offer_address, // the acceptor's address
            status: "accepted".to_string(),
            acceptor_refund_address: None,
        };

        record.acceptance = Some(acceptance);
        record.phase = "accepted".to_string();
        record.updated_at = now_epoch();

        persist_swap_store(&store)?;

        Ok(format!("Swap {} finalized", swap_id))
    })
    .await
    .map_err(|e| format!("Swap finalize task failed: {e}"))?
}

/// Check the status of a swap.
#[tauri::command]
pub async fn swap_status(swap_id: String) -> Result<SwapStatus, String> {
    let store = swap_store().lock().map_err(|_| "Swap store lock poisoned")?;
    let record = store
        .get(&swap_id)
        .ok_or_else(|| format!("Swap not found: {swap_id}"))?;

    Ok(SwapStatus {
        swap_id: swap_id.clone(),
        phase: record.phase.clone(),
        offer_chain: record.offer.offer_chain.clone(),
        offer_amount: record.offer.offer_amount,
        offer_asset: record.offer.offer_asset.clone(),
        offer_address: record.offer.offer_address.clone(),
        want_chain: record.offer.want_chain.clone(),
        want_amount: record.offer.want_amount,
        want_asset: record.offer.want_asset.clone(),
        want_address: record.offer.want_address.clone(),
        offer_htlc_funded: !record.offer.htlc_txid.is_empty(),
        offer_htlc_confirmed: false, // Would check on-chain
        accept_htlc_funded: record
            .acceptance
            .as_ref()
            .map(|a| !a.accept_htlc_txid.is_empty())
            .unwrap_or(false),
        accept_htlc_confirmed: false, // Would check on-chain
        secret_revealed: record.phase == "claimed" || record.phase == "completed",
        completed: record.phase == "completed",
        error: None,
        created_at: record.offer.created_at,
        updated_at: record.updated_at,
    })
}

/// List all swaps.
#[tauri::command]
pub async fn swap_list() -> Result<Vec<SwapSummary>, String> {
    let store = swap_store().lock().map_err(|_| "Swap store lock poisoned")?;
    let mut swaps: Vec<SwapSummary> = store
        .values()
        .map(|record| SwapSummary {
            swap_id: record.offer.swap_id.clone(),
            phase: record.phase.clone(),
            offer_chain: record.offer.offer_chain.clone(),
            offer_amount: record.offer.offer_amount,
            offer_asset: record.offer.offer_asset.clone(),
            offer_address: record.offer.offer_address.clone(),
            want_chain: record.offer.want_chain.clone(),
            want_amount: record.offer.want_amount,
            want_asset: record.offer.want_asset.clone(),
            want_address: record.offer.want_address.clone(),
            created_at: record.offer.created_at,
        })
        .collect();

    // Sort by created_at descending (newest first)
    swaps.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    Ok(swaps)
}

/// Claim funds from an HTLC by revealing the secret.
#[tauri::command]
pub async fn swap_claim(swap_id: String, provided_secret: Option<String>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut store = swap_store().lock().map_err(|_| "Swap store lock poisoned")?;
        let record = store
            .get_mut(&swap_id)
            .ok_or_else(|| format!("Swap not found: {swap_id}"))?;

        if record.phase != "accepted" {
            return Err("Swap must be in accepted phase to claim".to_string());
        }

        let is_initiator = record.secret.is_some();
        let secret_hex = if is_initiator {
            record.secret.clone().unwrap()
        } else {
            provided_secret.ok_or_else(|| "Acceptor must provide the secret to claim".to_string())?
        };

        let secret_bytes = hex::decode(&secret_hex)
            .map_err(|e| format!("Invalid secret hex: {e}"))?;

        let acceptance = record.acceptance.as_ref().ok_or("No acceptance found")?;

        let claim_chain;
        let claim_amount;
        let claim_to_address;
        let htlc_txid;
        let htlc_address;
        let mut htlc_script;

        if is_initiator {
            // Initiator claims on want_chain (Acceptor's HTLC)
            claim_chain = record.offer.want_chain.clone();
            claim_amount = record.offer.want_amount;
            claim_to_address = record.offer.want_address.clone();
            htlc_txid = acceptance.accept_htlc_txid.clone();
            htlc_address = acceptance.accept_htlc_address.clone();
            
            // Reconstruct acceptor's HTLC script
            let acceptor_refund = acceptance.acceptor_refund_address.clone().unwrap_or(acceptance.want_address.clone());
            let sender_pubkey_hash = extract_pubkey_hash(&acceptor_refund)?;
            let recipient_pubkey_hash = extract_pubkey_hash(&record.offer.want_address)?;
            let secret_hash_bytes = hex::decode(&record.offer.secret_hash)
                .map_err(|e| format!("Invalid secret hash: {e}"))?;
                
            let htlc_script_normal = build_htlc_script(&secret_hash_bytes, &recipient_pubkey_hash, &sender_pubkey_hash, acceptance.accept_locktime);
            let legacy_script = build_legacy_htlc_script(&secret_hash_bytes, &sender_pubkey_hash, acceptance.accept_locktime);
            
            let p2sh_normal = if claim_chain == "hemp" {
                crate::modules::atomic_swap::hemp_decodescript(&hex::encode(&htlc_script_normal)).unwrap_or_default()
            } else {
                crate::modules::atomic_swap::rvn_decodescript(&hex::encode(&htlc_script_normal)).unwrap_or_default()
            };
            
            let p2sh_legacy = if claim_chain == "hemp" {
                crate::modules::atomic_swap::hemp_decodescript(&hex::encode(&legacy_script)).unwrap_or_default()
            } else {
                crate::modules::atomic_swap::rvn_decodescript(&hex::encode(&legacy_script)).unwrap_or_default()
            };
            
            if htlc_address == p2sh_normal {
                htlc_script = htlc_script_normal;
            } else if htlc_address == p2sh_legacy {
                htlc_script = legacy_script;
            } else {
                htlc_script = htlc_script_normal;
            }
        } else {
            // Acceptor claims on offer_chain (Initiator's HTLC)
            claim_chain = record.offer.offer_chain.clone();
            claim_amount = record.offer.offer_amount;
            claim_to_address = acceptance.want_address.clone();
            htlc_txid = record.offer.htlc_txid.clone();
            htlc_address = record.offer.htlc_address.clone();

            // Reconstruct initiator's HTLC script
            let sender_pubkey_hash = extract_pubkey_hash(&record.offer.offer_address)?;
            let recipient_pubkey_hash = extract_pubkey_hash(&acceptance.want_address)?;
            let secret_hash_bytes = hex::decode(&record.offer.secret_hash)
                .map_err(|e| format!("Invalid secret hash: {e}"))?;
                
            let htlc_script_normal = build_htlc_script(&secret_hash_bytes, &recipient_pubkey_hash, &sender_pubkey_hash, record.offer.locktime);
            let legacy_script = build_legacy_htlc_script(&secret_hash_bytes, &sender_pubkey_hash, record.offer.locktime);
            
            let p2sh_normal = if claim_chain == "hemp" {
                crate::modules::atomic_swap::hemp_decodescript(&hex::encode(&htlc_script_normal)).unwrap_or_default()
            } else {
                crate::modules::atomic_swap::rvn_decodescript(&hex::encode(&htlc_script_normal)).unwrap_or_default()
            };
            
            let p2sh_legacy = if claim_chain == "hemp" {
                crate::modules::atomic_swap::hemp_decodescript(&hex::encode(&legacy_script)).unwrap_or_default()
            } else {
                crate::modules::atomic_swap::rvn_decodescript(&hex::encode(&legacy_script)).unwrap_or_default()
            };
            
            if htlc_address == p2sh_normal {
                htlc_script = htlc_script_normal;
            } else if htlc_address == p2sh_legacy {
                htlc_script = legacy_script;
            } else {
                htlc_script = htlc_script_normal;
            }
        }

        // 1. Find the UTXO
        let (vout, amount, _) = if claim_chain == "hemp" {
            hemp_find_utxo(&htlc_txid, &htlc_address)?
        } else {
            rvn_find_utxo(&htlc_txid, &htlc_address)?
        };

        // 2. Build the transaction output
        // We need to pay a small fee. Let's assume 0.01 fee for both chains to avoid min relay fee errors.
        let fee = 0.01;
        if amount <= fee {
            return Err("Amount too small to cover fee".to_string());
        }
        let send_amount = amount - fee;

        let inputs = serde_json::json!([{"txid": htlc_txid, "vout": vout}]);
        let outputs = serde_json::json!({claim_to_address.clone(): send_amount});

        // 3. Create raw transaction
        let raw_tx = if claim_chain == "hemp" {
            hemp_createrawtransaction(inputs, outputs)?
        } else {
            rvn_createrawtransaction(inputs, outputs)?
        };

        let mut htlc_address = record.offer.htlc_address.clone();
        if htlc_address.is_empty() {
            htlc_address = if claim_chain == "hemp" { record.offer.offer_address.clone() } else { record.offer.want_address.clone() };
        }
        let decoded = bs58::decode(&htlc_address).into_vec().map_err(|e| format!("Invalid P2SH: {e}"))?;
        let p2sh_hash = &decoded[1..21];
        let mut spk = vec![0xa9, 0x14]; // OP_HASH160, push 20 bytes
        spk.extend_from_slice(p2sh_hash);
        spk.push(0x87); // OP_EQUAL
        let spk_hex = hex::encode(&spk);

        // 4. Sign and Inject scriptSig
        let privkey_res = if claim_chain == "hemp" {
            crate::modules::rpc::call_rpc("dumpprivkey", &[serde_json::Value::String(claim_to_address.clone())])
        } else {
            crate::modules::rvn_rpc::call_rvn_rpc("dumpprivkey", &[serde_json::Value::String(claim_to_address.clone())])
        };
        let privkey_value = privkey_res.map_err(|e| format!("Wallet must be unlocked to sign: {e}"))?;
        let privkey = privkey_value.as_str().ok_or("Invalid WIF returned")?.to_string();

        let raw_tx_bytes = hex::decode(&raw_tx).map_err(|e| format!("Invalid hex tx: {e}"))?;
        let sighash = build_sighash_all(&raw_tx_bytes, &htlc_script)?;
        let (signature, pubkey) = sign_sighash(&privkey, &sighash)?;

        let script_sig = build_claim_script_sig(&signature, &pubkey, &secret_bytes, &htlc_script);
        let signed_tx = inject_script_sig(&raw_tx, &script_sig)?;

        // 5. Broadcast
        let txid = if claim_chain == "hemp" {
            hemp_sendrawtransaction(&signed_tx)?
        } else {
            rvn_sendrawtransaction(&signed_tx)?
        };

        record.phase = "claimed".to_string();
        record.updated_at = now_epoch();
        
        // If acceptor provided secret, save it
        if !is_initiator {
            record.secret = Some(secret_hex.clone());
        }

        persist_swap_store(&store)?;

        if is_initiator {
            Ok(format!("Claim successful! TXID: {txid}. IMPORTANT: Share this secret with the counterparty so they can claim their funds: {secret_hex}"))
        } else {
            Ok(format!("Claim successful! TXID: {txid}"))
        }
    })
    .await
    .map_err(|e| format!("Swap claim task failed: {e}"))?
}

/// Refund an expired HTLC.
#[tauri::command]
pub async fn swap_refund(swap_id: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut store = swap_store().lock().map_err(|_| "Swap store lock poisoned")?;
        let record = store
            .get_mut(&swap_id)
            .ok_or_else(|| format!("Swap not found: {swap_id}"))?;

        let is_initiator = record.secret.is_some();
        let offer = &record.offer;
        let acceptance = &record.acceptance;

        let refund_chain;
        let refund_amount;
        let refund_to_address;
        let htlc_txid;
        let htlc_address;
        let mut htlc_script;
        let locktime;
        
        let mut refund_signing_address = "".to_string();
        let mut refund_signing_chain = "".to_string();

        if is_initiator {
            // Initiator refunds their own HTLC on offer_chain
            refund_chain = offer.offer_chain.clone();
            refund_amount = offer.offer_amount;
            refund_to_address = offer.offer_address.clone();
            htlc_txid = offer.htlc_txid.clone();
            htlc_address = offer.htlc_address.clone();
            locktime = offer.locktime;

            let sender_pubkey_hash = extract_pubkey_hash(&offer.offer_address)?;
            let recipient_pubkey_hash = if let Some(acc) = &acceptance {
                extract_pubkey_hash(&acc.want_address)?
            } else {
                extract_pubkey_hash(&offer.want_address)?
            };
            let secret_hash_bytes = hex::decode(&offer.secret_hash)
                .map_err(|e| format!("Invalid secret hash: {e}"))?;
                
            let htlc_script_funded_by_offeror = build_htlc_script(&secret_hash_bytes, &sender_pubkey_hash, &sender_pubkey_hash, locktime);
            let legacy_script = build_legacy_htlc_script(&secret_hash_bytes, &sender_pubkey_hash, locktime);
            
            let p2sh_normal = crate::modules::atomic_swap::hemp_decodescript(&hex::encode(&htlc_script_funded_by_offeror)).unwrap_or_default();
            let p2sh_legacy = crate::modules::atomic_swap::hemp_decodescript(&hex::encode(&legacy_script)).unwrap_or_default();
            
            if offer.htlc_address == p2sh_normal {
                htlc_script = htlc_script_funded_by_offeror;
                refund_signing_address = refund_to_address.clone();
                refund_signing_chain = refund_chain.clone();
            } else if offer.htlc_address == p2sh_legacy {
                htlc_script = legacy_script;
                refund_signing_address = refund_to_address.clone();
                refund_signing_chain = refund_chain.clone();
            } else {
                // Wait, it might have been accepted, but if we're refunding our own HTLC, it shouldn't have changed.
                // If it DID change, try the other combinations just in case!
                let htlc_script_with_recipient = build_htlc_script(&secret_hash_bytes, &sender_pubkey_hash, &recipient_pubkey_hash, locktime);
                htlc_script = htlc_script_with_recipient;
                refund_signing_address = refund_to_address.clone();
                refund_signing_chain = refund_chain.clone();
            }
        } else {
            // Acceptor refunds their own HTLC on want_chain
            if acceptance.is_none() {
                return Err("No acceptance found to refund".to_string());
            }
            let acc = acceptance.as_ref().unwrap();
            refund_chain = offer.want_chain.clone();
            refund_amount = offer.want_amount;
            
            let acceptor_refund = acc.acceptor_refund_address.clone().unwrap_or(acc.want_address.clone());
            refund_to_address = acceptor_refund.clone();
            
            htlc_txid = acc.accept_htlc_txid.clone();
            htlc_address = acc.accept_htlc_address.clone();
            locktime = acc.accept_locktime;

            let sender_pubkey_hash = extract_pubkey_hash(&acceptor_refund)?;
            let recipient_pubkey_hash = extract_pubkey_hash(&offer.want_address)?;
            let secret_hash_bytes = hex::decode(&offer.secret_hash)
                .map_err(|e| format!("Invalid secret hash: {e}"))?;
                
            let htlc_script_normal = build_htlc_script(&secret_hash_bytes, &recipient_pubkey_hash, &sender_pubkey_hash, locktime);
            let legacy_script = build_legacy_htlc_script(&secret_hash_bytes, &sender_pubkey_hash, locktime);
            
            let p2sh_normal = if refund_chain == "hemp" {
                crate::modules::atomic_swap::hemp_decodescript(&hex::encode(&htlc_script_normal)).unwrap_or_default()
            } else {
                crate::modules::atomic_swap::rvn_decodescript(&hex::encode(&htlc_script_normal)).unwrap_or_default()
            };
            
            let p2sh_legacy = if refund_chain == "hemp" {
                crate::modules::atomic_swap::hemp_decodescript(&hex::encode(&legacy_script)).unwrap_or_default()
            } else {
                crate::modules::atomic_swap::rvn_decodescript(&hex::encode(&legacy_script)).unwrap_or_default()
            };
            
            if htlc_address == p2sh_normal {
                htlc_script = htlc_script_normal;
            } else if htlc_address == p2sh_legacy {
                htlc_script = legacy_script;
            } else {
                htlc_script = htlc_script_normal;
            }
            refund_signing_address = refund_to_address.clone();
            refund_signing_chain = refund_chain.clone();
        }

        let now = now_epoch();
        if now < locktime {
            return Err(format!(
                "Cannot refund yet. Locktime expires at {} (in {} seconds).",
                locktime,
                locktime - now
            ));
        }

        if htlc_txid.is_empty() {
            return Err("HTLC not funded yet".to_string());
        }

        // 1. Find the UTXO
        let (vout, amount, spk_hex) = if refund_chain == "hemp" {
            hemp_find_utxo(&htlc_txid, &htlc_address)?
        } else {
            rvn_find_utxo(&htlc_txid, &htlc_address)?
        };

        // 2. Build the transaction output
        let fee = 0.01;
        if amount <= fee {
            return Err("Amount too small to cover fee".to_string());
        }
        let send_amount = amount - fee;

        let inputs = serde_json::json!([{"txid": htlc_txid, "vout": vout, "sequence": locktime}]); // sequence must be >= locktime for OP_CHECKLOCKTIMEVERIFY
        let outputs = serde_json::json!({refund_to_address.clone(): send_amount});

        // For refund, set locktime field in the tx to `locktime`
        // createrawtransaction sets locktime to 0 by default. We can use a hack or build it.
        // Actually, Bitcoin's createrawtransaction supports a 3rd argument for locktime!
        let inputs_val = serde_json::json!([{"txid": htlc_txid, "vout": vout, "sequence": 4294967294u32}]); // 0xfffffffe enables nLockTime but disables BIP68
        
        let raw_tx = if refund_chain == "hemp" {
            let res = rpc::call_rpc("createrawtransaction", &[inputs_val, outputs.clone(), serde_json::json!(locktime)])?;
            res.as_str().ok_or("Failed to parse raw tx from Hemp node")?.to_string()
        } else {
            let res = rvn_rpc::call_rvn_rpc("createrawtransaction", &[inputs_val, outputs.clone(), serde_json::json!(locktime)])?;
            res.as_str().ok_or("Failed to parse raw tx from RVN node")?.to_string()
        };

        // 4. Sign and Inject scriptSig for Refund
        let privkey_res = if refund_signing_chain == "hemp" {
            crate::modules::rpc::call_rpc("dumpprivkey", &[serde_json::Value::String(refund_signing_address.clone())])
        } else {
            crate::modules::rvn_rpc::call_rvn_rpc("dumpprivkey", &[serde_json::Value::String(refund_signing_address.clone())])
        };
        let privkey_value = privkey_res.map_err(|e| format!("Wallet must be unlocked to sign: {e}"))?;
        let privkey = privkey_value.as_str().ok_or("Invalid WIF returned")?.to_string();

        let raw_tx_bytes = hex::decode(&raw_tx).map_err(|e| format!("Invalid hex tx: {e}"))?;
        let sighash = build_sighash_all(&raw_tx_bytes, &htlc_script)?;
        let (signature, pubkey) = sign_sighash(&privkey, &sighash)?;

        println!("DEBUG REFUND - raw_tx: {}", raw_tx);
        println!("DEBUG REFUND - htlc_script: {}", hex::encode(&htlc_script));
        println!("DEBUG REFUND - sighash: {}", hex::encode(&sighash));
        println!("DEBUG REFUND - signature: {}", hex::encode(&signature));
        println!("DEBUG REFUND - pubkey: {}", hex::encode(&pubkey));

        let refund_script_sig = build_refund_script_sig(&signature, &pubkey, &htlc_script);
        let signed_tx = inject_script_sig(&raw_tx, &refund_script_sig)?;
        
        println!("DEBUG REFUND - signed_tx: {}", signed_tx);

        // 5. Broadcast
        let txid = if refund_chain == "hemp" {
            hemp_sendrawtransaction(&signed_tx)?
        } else {
            rvn_sendrawtransaction(&signed_tx)?
        };

        record.phase = "refunded".to_string();
        record.updated_at = now_epoch();
        persist_swap_store(&store)?;

        Ok(format!("Refund successful! TXID: {txid}"))
    })
    .await
    .map_err(|e| format!("Swap refund task failed: {e}"))?
}

/// Delete a completed or cancelled swap from the store.
#[tauri::command]
pub async fn swap_delete(swap_id: String) -> Result<String, String> {
    let mut store = swap_store().lock().map_err(|_| "Swap store lock poisoned")?;
    let record = store
        .get(&swap_id)
        .ok_or_else(|| format!("Swap not found: {swap_id}"))?;

    if record.phase != "completed" && record.phase != "refunded" && record.phase != "created" {
        return Err("Can only delete completed, refunded, or unfunded swaps.".to_string());
    }

    store.remove(&swap_id);
    persist_swap_store(&store)?;

    Ok(format!("Swap {swap_id} deleted."))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_and_hash() {
        let secret = generate_secret();
        assert_eq!(secret.len(), 32);

        let hash = sha256_hash(&secret);
        assert_eq!(hash.len(), 32);

        // Same secret should produce the same hash
        let hash2 = sha256_hash(&secret);
        assert_eq!(hash, hash2);

        // Different secrets should produce different hashes
        let secret2 = generate_secret();
        let hash3 = sha256_hash(&secret2);
        assert_ne!(hash, hash3);
    }

    #[test]
    fn test_encode_script_num() {
        assert_eq!(encode_script_num(0), vec![0x00]);
        assert_eq!(encode_script_num(1), vec![0x01]);
        assert_eq!(encode_script_num(127), vec![0x7f]);
        assert_eq!(encode_script_num(128), vec![0x80, 0x00]);
        assert_eq!(encode_script_num(255), vec![0xff, 0x00]);
        assert_eq!(encode_script_num(256), vec![0x00, 0x01]);
    }

    #[test]
    fn test_htlc_script_length() {
        let secret_hash = vec![0u8; 32];
        let recipient = vec![0u8; 20];
        let sender = vec![0u8; 20];
        let locktime = 1700000000u64;

        let script = build_htlc_script(&secret_hash, &recipient, &sender, locktime);
        // Script should be well-formed and non-empty
        assert!(script.len() > 80);
        // Should start with OP_IF (0x63)
        assert_eq!(script[0], 0x63);
        // Should end with OP_ENDIF (0x68)
        assert_eq!(*script.last().unwrap(), 0x68);
    }
}
