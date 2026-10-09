use chrono::Local;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

// Import local modules
use crate::modules::models::{
    AddressBookEntry, AppSettings, BinaryStatus, ConfigChange, ConfigHelpEntry, ConfigHelpSection,
    ConfigPaths, ConfigPreview, DataFolderInfo, DataMovePreview, DataMoveResult, RepairStatus,
};
use crate::modules::utils::{
    bin_name, calculate_dir_size, format_size, resolve_bin, resolve_bin_with_override,
};

pub fn default_core_data_dir() -> Result<PathBuf, String> {
    if cfg!(windows) {
        let appdata = std::env::var("APPDATA").map_err(|_| "APPDATA not set".to_string())?;
        Ok(PathBuf::from(appdata).join("Hemp0x"))
    } else {
        let home = dirs::home_dir().ok_or("HOME not set")?;
        Ok(home.join(".hemp0x"))
    }
}

// Bootstrap dir is always under default_core_data_dir — never under active data dir
fn bootstrap_dir() -> Result<PathBuf, String> {
    Ok(default_core_data_dir()?.join("commander"))
}

fn bootstrap_path() -> Result<PathBuf, String> {
    Ok(bootstrap_dir()?.join("bootstrap.json"))
}

fn load_bootstrap() -> Result<serde_json::Value, String> {
    let path = bootstrap_path()?;
    if path.exists() {
        let content = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let val: serde_json::Value = serde_json::from_str(&content).unwrap_or_else(|_| {
            let mut map = serde_json::Map::new();
            map.insert("custom_data_dir".to_string(), serde_json::Value::Null);
            serde_json::Value::Object(map)
        });
        return Ok(val);
    }
    let mut map = serde_json::Map::new();
    map.insert("custom_data_dir".to_string(), serde_json::Value::Null);
    Ok(serde_json::Value::Object(map))
}

fn save_bootstrap(custom_data_dir: Option<String>) -> Result<(), String> {
    let path = bootstrap_path()?;
    let dir = path
        .parent()
        .ok_or("Could not determine bootstrap parent directory")?;
    if !dir.exists() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut map = serde_json::Map::new();
    map.insert(
        "custom_data_dir".to_string(),
        match custom_data_dir {
            Some(s) => serde_json::Value::String(s),
            None => serde_json::Value::Null,
        },
    );
    let content =
        serde_json::to_string_pretty(&serde_json::Value::Object(map)).map_err(|e| e.to_string())?;
    fs::write(&path, content).map_err(|e| e.to_string())?;
    Ok(())
}

// Active data dir follows the bootstrap pointer
pub fn active_data_dir() -> Result<PathBuf, String> {
    #[cfg(test)]
    {
        let override_path = TEST_DATA_DIR.with(|cell| cell.borrow().clone());
        if let Some(dir) = override_path {
            // The override is responsible for creating the directory.
            return Ok(dir);
        }
    }
    let bootstrap = load_bootstrap()?;
    if let Some(serde_json::Value::String(ref custom)) = bootstrap.get("custom_data_dir") {
        let p = PathBuf::from(custom);
        if !p.is_absolute() {
            return Err(
                "Custom data directory must be an absolute path. Use Settings to fix or reset."
                    .to_string(),
            );
        }
        if p.exists() && p.is_file() {
            return Err("Custom data directory points to a file, not a directory. Use Settings to fix or reset.".to_string());
        }
        return Ok(p);
    }
    default_core_data_dir()
}

// Legacy alias: data_dir() is the active data directory
pub fn data_dir() -> Result<PathBuf, String> {
    active_data_dir()
}

#[cfg(test)]
thread_local! {
    pub static TEST_DATA_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

// Active commander settings live under the active data dir. Routed
// through commander_dir() so that in tests the settings file resolves
// to the isolated TEST_COMMANDER_DIR instead of the user's real
// ~/.hemp0x/commander/app_settings.json (which must never be mutated
// by the test suite).
pub fn commander_settings_path() -> Result<PathBuf, String> {
    Ok(commander_dir()?.join("app_settings.json"))
}

#[cfg(test)]
thread_local! {
    pub static TEST_COMMANDER_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

pub fn commander_dir() -> Result<PathBuf, String> {
    #[cfg(test)]
    {
        let override_path = TEST_COMMANDER_DIR.with(|cell| cell.borrow().clone());
        if let Some(dir) = override_path {
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            return Ok(dir);
        }
    }
    let dir = active_data_dir()?.join("commander");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

pub fn commander_content_library_dir() -> Result<PathBuf, String> {
    Ok(commander_dir()?.join("content-library"))
}

fn external_slice17_settings_path() -> Result<PathBuf, String> {
    if cfg!(windows) {
        let appdata = std::env::var("APPDATA").map_err(|_| "APPDATA not set".to_string())?;
        Ok(PathBuf::from(appdata)
            .join("Hemp0xCommander")
            .join("app_settings.json"))
    } else if cfg!(target_os = "macos") {
        let home = dirs::home_dir().ok_or("HOME not set")?;
        Ok(home
            .join("Library")
            .join("Application Support")
            .join("Hemp0xCommander")
            .join("app_settings.json"))
    } else {
        let config = dirs::config_dir().ok_or("Could not determine config directory")?;
        Ok(config.join("hemp0x-commander").join("app_settings.json"))
    }
}

fn fallback_prior_17b_settings_path() -> Result<PathBuf, String> {
    // Slice 17b stored settings under default dir regardless of custom dir
    Ok(default_core_data_dir()?
        .join("commander")
        .join("app_settings.json"))
}

pub fn load_app_settings_impl() -> Result<AppSettings, String> {
    let active_path = commander_settings_path()?;

    if active_path.exists() {
        let content = fs::read_to_string(&active_path).map_err(|e| e.to_string())?;
        return Ok(serde_json::from_str(&content).unwrap_or_default());
    }

    // Fallback 1: prior 17b path (default dir /commander/app_settings.json)
    if let Ok(prior17b) = fallback_prior_17b_settings_path() {
        if prior17b.exists() {
            let content = fs::read_to_string(&prior17b).map_err(|e| e.to_string())?;
            let settings: AppSettings = serde_json::from_str(&content).unwrap_or_default();
            save_app_settings_impl(&settings)?;
            return Ok(settings);
        }
    }

    // Fallback 2: external Slice 17 path
    if let Ok(external) = external_slice17_settings_path() {
        if external.exists() {
            let content = fs::read_to_string(&external).map_err(|e| e.to_string())?;
            let settings: AppSettings = serde_json::from_str(&content).unwrap_or_default();
            save_app_settings_impl(&settings)?;
            return Ok(settings);
        }
    }

    // Fallback 3: legacy root path
    let legacy_path = default_core_data_dir()?.join("app_settings.json");
    if legacy_path.exists() {
        let content = fs::read_to_string(&legacy_path).map_err(|e| e.to_string())?;
        let settings: AppSettings = serde_json::from_str(&content).unwrap_or_default();
        save_app_settings_impl(&settings)?;
        return Ok(settings);
    }

    Ok(AppSettings::default())
}

pub fn save_app_settings_impl(settings: &AppSettings) -> Result<(), String> {
    let path = commander_settings_path()?;
    let cfg_dir = path
        .parent()
        .ok_or("Could not determine settings parent directory")?;
    if !cfg_dir.exists() {
        fs::create_dir_all(cfg_dir).map_err(|e| e.to_string())?;
    }
    let content = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(&path, content).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn config_path() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("hemp.conf"))
}

fn data_dir_has_existing_core_state(dir: &Path) -> bool {
    [
        "wallet.dat",
        "blocks",
        "chainstate",
        "debug.log",
        "peers.dat",
        "fee_estimates.dat",
    ]
    .iter()
    .any(|name| dir.join(name).exists())
}

pub fn ensure_config() -> Result<PathBuf, String> {
    let dir = data_dir()?;
    let cfg = config_path()?;
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    if !cfg.exists() {
        if data_dir_has_existing_core_state(&dir) {
            // AUDIT: surface the refusal so it is visible in the app log,
            // not just as an error returned to one caller.
            log::warn!(
        "ensure_config refused to auto-create hemp.conf in established Core data dir {} (existing Core state present, no hemp.conf). Restoring a tuned config requires the user's backup or an explicit Create Default.",
        dir.to_string_lossy()
      );
            return Err(format!(
        "hemp.conf is missing from existing Core data directory {}. Commander refused to create a replacement default because that could hide or overwrite a tuned node configuration. Restore your hemp.conf backup or use System > Config > Create Default intentionally.",
        dir.to_string_lossy()
      ));
        }
        let daemon_flag = "0";
        let content = format!(
            "# Hemp0x Configuration\n\
       # Core cookie auth is used by default; rpcuser/rpcpassword are not required.\n\
       server=1\n\
       rpcport=42068\n\
       daemon={}\n\
       addnode=154.38.164.123:42069\n\
       addnode=147.93.185.184:42069\n",
            daemon_flag
        );
        fs::write(&cfg, content).map_err(|e| e.to_string())?;
    } else {
        // Ensure existing hemp.conf sets rpcport=42068 so it never defaults to 8766
        // and collides with the Ravencoin daemon.
        if let Ok(mut content) = fs::read_to_string(&cfg) {
            if !content.lines().any(|l| l.trim().starts_with("rpcport=")) {
                content.push_str("\nrpcport=42068\n");
                let _ = fs::write(&cfg, content);
            }
        }
    }
    Ok(cfg)
}

pub fn parse_config(path: &Path) -> Result<HashMap<String, String>, String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_config_from_str(&content)
}

fn parse_config_from_str(content: &str) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.insert(
                k.trim().to_string(),
                config_value_without_inline_comment(v).to_string(),
            );
        }
    }
    Ok(map)
}

fn parse_config_multi(content: &str) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            map.entry(k.trim().to_string())
                .or_default()
                .push(config_value_without_inline_comment(v).to_string());
        }
    }
    map
}

const SECRET_KEYS: &[&str] = &["rpcpassword", "rpcuser", "rpcauth"];
const SINGLETON_KEYS: &[&str] = &[
    "server",
    "listen",
    "daemon",
    "dbcache",
    "maxconnections",
    "prune",
    "txindex",
    "addressindex",
    "assetindex",
    "timestampindex",
    "spentindex",
    "messageindex",
    "zmqpubrawtx",
    "zmqpubhashblock",
    "zmqpubhashtx",
    "zmqpubrawblock",
    "rpcport",
    "port",
    "rpcallowip",
    "proxy",
];
const MULTI_KEYS: &[&str] = &["addnode", "connect", "seednode"];

fn is_secret_key(key: &str) -> bool {
    let k = key.to_lowercase();
    SECRET_KEYS.iter().any(|&s| k == s)
}

fn is_singleton(key: &str) -> bool {
    let k = key.to_lowercase();
    SINGLETON_KEYS.iter().any(|&s| k == s)
}

fn is_multi(key: &str) -> bool {
    let k = key.to_lowercase();
    MULTI_KEYS.iter().any(|&s| k == s)
}

fn config_value_without_inline_comment(value: &str) -> &str {
    let trimmed = value.trim();
    let comment_index = trimmed
        .char_indices()
        .find(|(index, ch)| {
            *ch == '#'
                && (*index == 0
                    || trimmed[..*index]
                        .chars()
                        .last()
                        .map(char::is_whitespace)
                        .unwrap_or(false))
        })
        .map(|(index, _)| index);
    comment_index
        .map(|index| trimmed[..index].trim_end())
        .unwrap_or(trimmed)
}

#[derive(Clone, PartialEq, Debug)]
enum ConfigLine {
    Comment {
        raw: String,
        ending: String,
    },
    Blank {
        raw: String,
        ending: String,
    },
    Option {
        key: String,
        value: String,
        raw: String,
        ending: String,
    },
    Unknown {
        raw: String,
        ending: String,
    },
}

fn detect_newline(content: &[u8]) -> &str {
    if content.windows(2).any(|w| w == b"\r\n") {
        return "\r\n";
    }
    "\n"
}

fn tokenize_config(content: &str) -> Vec<ConfigLine> {
    let nl = detect_newline(content.as_bytes());
    let has_trailing_nl = content.ends_with(nl);
    let content_no_trailing = if has_trailing_nl {
        &content[..content.len() - nl.len()]
    } else {
        content
    };

    let lines: Vec<&str> = if content_no_trailing.is_empty() {
        vec![]
    } else {
        content_no_trailing.split(nl).collect()
    };

    let total = lines.len();
    let mut result = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let is_last = i + 1 == total;
        let ending = if is_last {
            if has_trailing_nl {
                nl
            } else {
                ""
            }
        } else {
            nl
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            result.push(ConfigLine::Blank {
                raw: (*line).to_string(),
                ending: ending.to_string(),
            });
            continue;
        }
        if trimmed.starts_with('#') {
            result.push(ConfigLine::Comment {
                raw: (*line).to_string(),
                ending: ending.to_string(),
            });
            continue;
        }
        if let Some((k, v)) = line.trim_start().split_once('=') {
            result.push(ConfigLine::Option {
                key: k.trim().to_string(),
                value: config_value_without_inline_comment(v).to_string(),
                raw: (*line).to_string(),
                ending: ending.to_string(),
            });
        } else {
            result.push(ConfigLine::Unknown {
                raw: (*line).to_string(),
                ending: ending.to_string(),
            });
        }
    }

    if result.is_empty() && has_trailing_nl {
        result.push(ConfigLine::Blank {
            raw: String::new(),
            ending: String::new(),
        });
    }
    result
}

fn reconstruct_config(lines: &[ConfigLine]) -> String {
    let mut out = String::new();
    for line in lines {
        match line {
            ConfigLine::Comment { raw, ending }
            | ConfigLine::Blank { raw, ending }
            | ConfigLine::Unknown { raw, ending } => {
                out.push_str(raw);
                out.push_str(ending);
            }
            ConfigLine::Option { raw, ending, .. } => {
                out.push_str(raw);
                out.push_str(ending);
            }
        }
    }
    out
}

fn replace_option_value(raw: &str, new_value: &str) -> String {
    let Some(eq_index) = raw.find('=') else {
        return raw.to_string();
    };
    let value_start = eq_index + 1;
    let after_equals = &raw[value_start..];
    let leading_value_ws_len = after_equals.len() - after_equals.trim_start().len();
    let value_and_comment = &after_equals[leading_value_ws_len..];
    let comment_index = value_and_comment
        .char_indices()
        .find(|(index, ch)| {
            *ch == '#'
                && (*index == 0
                    || value_and_comment[..*index]
                        .chars()
                        .last()
                        .map(char::is_whitespace)
                        .unwrap_or(false))
        })
        .map(|(index, _)| index);
    let comment_suffix = comment_index
        .map(|index| {
            let value_part = &value_and_comment[..index];
            let trailing_ws_len = value_part.len() - value_part.trim_end().len();
            format!(
                "{}{}",
                &value_part[value_part.len() - trailing_ws_len..],
                &value_and_comment[index..]
            )
        })
        .unwrap_or_default();

    format!(
        "{}{}{}{}",
        &raw[..value_start],
        &after_equals[..leading_value_ws_len],
        new_value,
        comment_suffix
    )
}

fn apply_config_changes(original: &str, changes: &HashMap<String, Option<String>>) -> String {
    let mut lines = tokenize_config(original);

    lines.retain(|line| {
        !matches!(
            line,
            ConfigLine::Option { key, .. }
                if !is_multi(key) && matches!(changes.get(key), Some(None))
        )
    });

    for line in lines.iter_mut() {
        if let ConfigLine::Option {
            key, value, raw, ..
        } = line
        {
            if let Some(new_opt) = changes.get(key) {
                if is_multi(key) {
                    continue; // Handled separately
                }
                if let Some(new_val) = new_opt {
                    if *value == *new_val {
                        continue;
                    }
                    *raw = replace_option_value(raw, new_val);
                    *value = new_val.clone();
                }
            }
        }
    }

    // Add new singleton keys that weren't present
    let existing_keys: Vec<String> = lines
        .iter()
        .filter_map(|l| {
            if let ConfigLine::Option { key, .. } = l {
                Some(key.clone())
            } else {
                None
            }
        })
        .collect();

    let nls: Vec<String> = changes
        .iter()
        .filter(|(k, v)| !existing_keys.contains(k) && is_singleton(k) && v.is_some())
        .map(|(k, _)| k.clone())
        .collect();

    if !nls.is_empty() {
        let nl = detect_newline(original.as_bytes()).to_string();
        if !lines.is_empty() {
            let last_ending = match &lines[lines.len() - 1] {
                ConfigLine::Comment { ending, .. }
                | ConfigLine::Blank { ending, .. }
                | ConfigLine::Option { ending, .. }
                | ConfigLine::Unknown { ending, .. } => ending.clone(),
            };
            if last_ending.is_empty() {
                lines.push(ConfigLine::Blank {
                    raw: String::new(),
                    ending: nl.clone(),
                });
            }
        }
        for key in &nls {
            if let Some(Some(val)) = changes.get(key) {
                let raw = format!("{}={}", key, val);
                lines.push(ConfigLine::Option {
                    key: key.clone(),
                    value: val.clone(),
                    raw,
                    ending: nl.clone(),
                });
            }
        }
    }

    // Handle addnode changes
    if let Some(Some(addnode_val)) = changes.get("addnode") {
        // Remove all existing addnode lines
        lines.retain(|l| !matches!(l, ConfigLine::Option { key, .. } if key == "addnode"));

        // Write fresh addnode entries, one per line
        let nl = detect_newline(original.as_bytes()).to_string();
        for entry in addnode_val.split(',') {
            let entry = entry.trim();
            if !entry.is_empty() {
                let raw = format!("addnode={}", entry);
                lines.push(ConfigLine::Option {
                    key: "addnode".to_string(),
                    value: entry.to_string(),
                    raw,
                    ending: nl.clone(),
                });
            }
        }
    }

    reconstruct_config(&lines)
}

fn addnode_entry_in_list(entries: &[String], hostport: &str) -> bool {
    let hostport = hostport.trim();
    entries.iter().any(|e| e.trim() == hostport)
}

fn detect_config_changes(
    old_content: &str,
    changes: &HashMap<String, Option<String>>,
) -> Vec<ConfigChange> {
    let old_map = parse_config_from_str(old_content).unwrap_or_default();
    let old_multi = parse_config_multi(old_content);
    let mut result = Vec::new();
    for (key, new_val_opt) in changes {
        if is_multi(key) {
            let old_entries = old_multi.get(key).cloned().unwrap_or_default();
            let new_str = match new_val_opt {
                Some(v) => v.clone(),
                None => String::new(),
            };
            let new_entries: Vec<String> = if new_str.is_empty() {
                Vec::new()
            } else {
                new_str
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            };
            let added: Vec<_> = new_entries
                .iter()
                .filter(|e| !addnode_entry_in_list(&old_entries, e))
                .cloned()
                .collect();
            let removed: Vec<_> = old_entries
                .iter()
                .filter(|e| !addnode_entry_in_list(&new_entries, e))
                .cloned()
                .collect();
            if !added.is_empty() || !removed.is_empty() {
                result.push(ConfigChange {
                    key: key.clone(),
                    old_value: if old_entries.is_empty() {
                        None
                    } else {
                        Some(old_entries.join(", "))
                    },
                    new_value: if new_entries.is_empty() {
                        None
                    } else {
                        Some(new_entries.join(", "))
                    },
                    action: if removed.is_empty() {
                        "added".to_string()
                    } else if added.is_empty() {
                        "removed".to_string()
                    } else {
                        "changed".to_string()
                    },
                });
            }
            continue;
        }
        let old_val = old_map.get(key).cloned();
        match (old_val, new_val_opt) {
            (None, Some(new)) => {
                result.push(ConfigChange {
                    key: key.clone(),
                    old_value: None,
                    new_value: Some(new.clone()),
                    action: "added".to_string(),
                });
            }
            (Some(old), None) => {
                result.push(ConfigChange {
                    key: key.clone(),
                    old_value: Some(old),
                    new_value: None,
                    action: "removed".to_string(),
                });
            }
            (Some(old), Some(new)) if old != *new => {
                result.push(ConfigChange {
                    key: key.clone(),
                    old_value: Some(old),
                    new_value: Some(new.clone()),
                    action: "changed".to_string(),
                });
            }
            _ => {}
        }
    }
    result
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn validate_addnode_entry(entry: &str) -> Result<String, String> {
    let entry = entry.trim();
    if entry.is_empty() {
        return Err("empty addnode entry".to_string());
    }
    // Handle bracketed IPv6: [::1]:port
    if entry.starts_with('[') {
        if let Some(bracket_end) = entry.find(']') {
            let ip_part = &entry[1..bracket_end];
            let rest = entry[bracket_end + 1..].trim();
            if rest.is_empty() || !rest.starts_with(':') {
                return Err(format!(
                    "malformed addnode entry: missing port in '{}'",
                    entry
                ));
            }
            let port_str = &rest[1..];
            if port_str.parse::<u16>().is_err() {
                return Err(format!(
                    "malformed addnode entry: invalid port in '{}'",
                    entry
                ));
            }
            if ip_part.is_empty() {
                return Err(format!(
                    "malformed addnode entry: empty IPv6 in '{}'",
                    entry
                ));
            }
            return Ok(format!("{}:{}", ip_part, port_str));
        }
        return Err(format!(
            "malformed addnode entry: unmatched '[' in '{}'",
            entry
        ));
    }
    // host:port or ip:port
    let parts: Vec<&str> = entry.rsplitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(format!(
            "malformed addnode entry: missing port in '{}'",
            entry
        ));
    }
    let host = parts[1].trim();
    let port_str = parts[0].trim();
    if host.is_empty() {
        return Err(format!(
            "malformed addnode entry: empty host in '{}'",
            entry
        ));
    }
    if port_str.parse::<u16>().is_err() {
        return Err(format!(
            "malformed addnode entry: invalid port in '{}'",
            entry
        ));
    }
    Ok(entry.to_string())
}

fn validate_zmq_endpoint(endpoint: &str) -> Result<(), String> {
    let e = endpoint.trim();
    if e.is_empty() {
        return Ok(());
    }
    let lower = e.to_lowercase();
    for prefix in ["tcp://127.0.0.1:", "tcp://localhost:", "tcp://[::1]:"] {
        if let Some(port) = lower.strip_prefix(prefix) {
            return match port.parse::<u16>() {
                Ok(port) if port > 0 => Ok(()),
                _ => Err(format!(
                    "ZMQ endpoint '{}' must use a valid localhost TCP port.",
                    e
                )),
            };
        }
    }
    if let Some(path) = e.strip_prefix("ipc://") {
        return if path.trim().is_empty() {
            Err("ZMQ IPC endpoint must include a local path.".to_string())
        } else {
            Ok(())
        };
    }
    Err(format!(
        "ZMQ endpoint '{}' is not local. Commander's guided config only supports localhost TCP or local IPC endpoints.",
        e
    ))
}

fn validate_config_settings(settings: &HashMap<String, String>) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let prune_val = settings.get("prune").and_then(|v| v.parse::<u64>().ok());
    let txindex = settings.get("txindex").and_then(|v| parse_bool(v));

    if let (Some(prune), Some(true)) = (prune_val, txindex) {
        if prune > 0 {
            errors.push(
                "prune > 0 is incompatible with txindex=1. Core will refuse to start. Either set prune=0 or txindex=0.".to_string(),
            );
        }
    }

    if let Some(prune) = prune_val {
        if prune > 1 && prune < 550 {
            errors.push(format!(
                "prune={} is below the Core minimum of 550 MiB. Node startup will fail. Use 0 (off), 1 (manual), or >=550 (auto).",
                prune
            ));
        }
    }

    if let Some(v) = settings.get("dbcache") {
        match v.parse::<u64>() {
            Ok(n) => {
                if n < 4 {
                    errors.push(format!("dbcache={} is below the minimum of 4 MB.", n));
                }
            }
            Err(_) => {
                errors.push(format!("dbcache='{}' is not a valid number.", v));
            }
        }
    }

    if let Some(v) = settings.get("maxconnections") {
        match v.parse::<u64>() {
            Ok(n) => {
                if n == 0 {
                    errors.push("maxconnections cannot be 0.".to_string());
                }
            }
            Err(_) => {
                errors.push(format!("maxconnections='{}' is not a valid number.", v));
            }
        }
    }

    // Validate addnode entries (now stored individually in multi-valued parse)
    if let Some(addnode_vals) = settings.get("addnode") {
        for entry in addnode_vals.split(',') {
            let entry = entry.trim();
            if !entry.is_empty() {
                if let Err(e) = validate_addnode_entry(entry) {
                    errors.push(e);
                }
            }
        }
    }

    if let Some(zmq) = settings.get("zmqpubrawtx") {
        if let Err(error) = validate_zmq_endpoint(zmq) {
            errors.push(error);
        }
    }

    // Detect conflicting duplicate singletons
    // This is checked in the parsed map — HashMap inherently deduplicates,
    // so if there were duplicates in the file, only the last survives.
    // We warn if server=0 is set in a Commander-managed context.
    if let Some(v) = settings.get("server") {
        if parse_bool(v) == Some(false) {
            warnings.push(
                "server=0 disables JSON-RPC. Commander requires server=1 for node control. The Full Feature and Storage Saver presets set server=1.".to_string(),
            );
        }
    }

    (errors, warnings)
}

fn duplicate_singleton_errors(content: &str) -> Vec<String> {
    parse_config_multi(content)
        .into_iter()
        .filter(|(key, values)| is_singleton(key) && values.len() > 1)
        .filter_map(|(key, values)| {
            let unique: std::collections::HashSet<_> =
                values.iter().map(|value| value.trim()).collect();
            (unique.len() > 1).then(|| {
                format!(
                    "Conflicting duplicate '{}' settings are active. Resolve them in the raw editor before applying guided changes.",
                    key
                )
            })
        })
        .collect()
}

fn sectioned_config_error(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let trimmed = line.trim();
        (trimmed.starts_with('[') && trimmed.ends_with(']')).then(|| {
            "Guided configuration cannot safely edit a hemp.conf that uses network sections. Use the raw editor so section-specific settings remain unchanged.".to_string()
        })
    })
}

fn validate_config_content(content: &str) -> (Vec<String>, Vec<String>) {
    let parsed = parse_config_from_str(content).unwrap_or_default();
    let (mut errors, warnings) = validate_config_settings(&parsed);
    if let Some(error) = sectioned_config_error(content) {
        errors.push(error);
    }
    errors.extend(duplicate_singleton_errors(content));
    for entry in parse_config_multi(content)
        .get("addnode")
        .into_iter()
        .flatten()
    {
        if let Err(error) = validate_addnode_entry(entry) {
            if !errors.contains(&error) {
                errors.push(error);
            }
        }
    }
    (errors, warnings)
}

fn detect_reindex_requirements(
    old_settings: &HashMap<String, String>,
    new_settings: &HashMap<String, String>,
) -> (Vec<String>, Vec<String>) {
    let mut full_reindex = Vec::new();
    let mut chainstate_reindex = Vec::new();

    let old_txindex = old_settings
        .get("txindex")
        .and_then(|v| parse_bool(v))
        .unwrap_or(false);
    let new_txindex = new_settings
        .get("txindex")
        .and_then(|v| parse_bool(v))
        .unwrap_or(false);
    if old_txindex != new_txindex {
        full_reindex.push(format!(
            "txindex changed: {} -> {} (requires -reindex)",
            old_txindex, new_txindex
        ));
    }

    let old_assetindex = old_settings
        .get("assetindex")
        .and_then(|v| parse_bool(v))
        .unwrap_or(false);
    let new_assetindex = new_settings
        .get("assetindex")
        .and_then(|v| parse_bool(v))
        .unwrap_or(false);
    if old_assetindex != new_assetindex {
        full_reindex.push(format!(
            "assetindex changed: {} -> {} (requires -reindex)",
            old_assetindex, new_assetindex
        ));
    }

    let old_addressindex = old_settings
        .get("addressindex")
        .and_then(|v| parse_bool(v))
        .unwrap_or(false);
    let new_addressindex = new_settings
        .get("addressindex")
        .and_then(|v| parse_bool(v))
        .unwrap_or(false);
    if old_addressindex != new_addressindex {
        chainstate_reindex.push(format!(
            "addressindex changed: {} -> {} (requires -reindex-chainstate)",
            old_addressindex, new_addressindex
        ));
    }

    let old_spentindex = old_settings
        .get("spentindex")
        .and_then(|v| parse_bool(v))
        .unwrap_or(false);
    let new_spentindex = new_settings
        .get("spentindex")
        .and_then(|v| parse_bool(v))
        .unwrap_or(false);
    if old_spentindex != new_spentindex {
        chainstate_reindex.push(format!(
            "spentindex changed: {} -> {} (requires -reindex-chainstate)",
            old_spentindex, new_spentindex
        ));
    }

    let old_timestampindex = old_settings
        .get("timestampindex")
        .and_then(|v| parse_bool(v))
        .unwrap_or(false);
    let new_timestampindex = new_settings
        .get("timestampindex")
        .and_then(|v| parse_bool(v))
        .unwrap_or(false);
    if old_timestampindex != new_timestampindex {
        chainstate_reindex.push(format!(
            "timestampindex changed: {} -> {} (requires -reindex-chainstate)",
            old_timestampindex, new_timestampindex
        ));
    }

    (full_reindex, chainstate_reindex)
}

fn compute_preview_token(content: &str, changes: &HashMap<String, Option<String>>) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let mut ordered: Vec<_> = changes.iter().collect();
    ordered.sort_by(|(left, _), (right, _)| left.cmp(right));
    for (key, value) in ordered {
        hasher.update([0]);
        hasher.update(key.as_bytes());
        hasher.update([0]);
        match value {
            Some(value) => {
                hasher.update([1]);
                hasher.update(value.as_bytes());
            }
            None => hasher.update([2]),
        }
    }
    hex::encode(hasher.finalize())
}

#[tauri::command]
pub fn parse_current_config() -> Result<HashMap<String, String>, String> {
    let cfg = config_path()?;
    if !cfg.exists() {
        return Ok(HashMap::new());
    }
    let content = fs::read_to_string(&cfg).map_err(|e| e.to_string())?;
    let parsed = parse_config_from_str(&content)?;
    Ok(parsed
        .into_iter()
        .filter(|(key, _)| {
            matches!(
                key.as_str(),
                "server"
                    | "listen"
                    | "daemon"
                    | "dbcache"
                    | "maxconnections"
                    | "prune"
                    | "txindex"
                    | "addressindex"
                    | "assetindex"
                    | "timestampindex"
                    | "spentindex"
                    | "messageindex"
                    | "zmqpubrawtx"
            )
        })
        .collect())
}

#[tauri::command]
pub fn get_addnode_hosts() -> Result<Vec<String>, String> {
    let cfg = config_path()?;
    if !cfg.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&cfg).map_err(|e| e.to_string())?;
    let multi = parse_config_multi(&content);
    let addnodes = multi.get("addnode").cloned().unwrap_or_default();
    let mut hosts = Vec::new();
    for entry in &addnodes {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        // Extract host from host:port, handling bracketed IPv6
        let host = if entry.starts_with('[') {
            entry
                .split(']')
                .next()
                .map(|s| &s[1..])
                .unwrap_or(entry)
                .to_string()
        } else {
            entry.rsplit(':').skip(1).collect::<Vec<&str>>().join(":")
        };
        let host = host.trim();
        if !host.is_empty() {
            hosts.push(host.to_string());
        }
    }
    Ok(hosts)
}

#[tauri::command]
pub fn preview_config_changes(
    changes: HashMap<String, Option<String>>,
) -> Result<ConfigPreview, String> {
    let cfg = config_path()?;
    let original = if cfg.exists() {
        fs::read_to_string(&cfg).map_err(|e| e.to_string())?
    } else {
        String::new()
    };

    let applied = apply_config_changes(&original, &changes);
    let config_changes = detect_config_changes(&original, &changes);
    let new_parsed = parse_config_from_str(&applied).unwrap_or_default();
    let old_parsed = parse_config_from_str(&original).unwrap_or_default();

    let (mut validation_errors, validation_warnings) = validate_config_content(&applied);

    if applied.trim().is_empty() {
        validation_errors.push("Resulting config would be empty. Refusing to apply.".to_string());
    }

    let (reindex, reindex_chainstate) = detect_reindex_requirements(&old_parsed, &new_parsed);
    let restart_required = !config_changes.is_empty();
    let preview_token = compute_preview_token(&original, &changes);

    // Redact secret values in changes
    let redacted_changes: Vec<ConfigChange> = config_changes
        .iter()
        .map(|c| {
            let mut rc = c.clone();
            if is_secret_key(&c.key) {
                rc.old_value = c.old_value.as_ref().map(|_| "***REDACTED***".to_string());
                rc.new_value = c.new_value.as_ref().map(|_| "***REDACTED***".to_string());
            }
            rc
        })
        .collect();

    Ok(ConfigPreview {
        changes: redacted_changes,
        validation_warnings,
        validation_errors,
        reindex_required: reindex,
        reindex_chainstate_required: reindex_chainstate,
        restart_required,
        preview_token,
    })
}

fn atomic_write_config(content: &str) -> Result<(), String> {
    let cfg = config_path()?;
    let dir = cfg.parent().ok_or("Config path has no parent directory")?;
    let now = chrono::Utc::now();
    let timestamp = now.format("%Y%m%d-%H%M%S");
    let nanos = now.timestamp_subsec_nanos();

    // Create timestamped backup first
    if cfg.exists() {
        // Avoid same-second collisions by appending nanos
        let backup_name = format!("hemp.conf.{}.{:09}.bak", timestamp, nanos);
        let backup_path = dir.join(&backup_name);
        fs::copy(&cfg, &backup_path)
            .map_err(|e| format!("Failed to create config backup: {}", e))?;
    }

    // Write to temp file, then rename atomically
    let temp_path = dir.join(format!(".hemp.conf.{}.{:09}.tmp", timestamp, nanos));
    let mut temp_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .map_err(|e| format!("Failed to create temporary config: {}", e))?;
    temp_file
        .write_all(content.as_bytes())
        .and_then(|_| temp_file.sync_all())
        .map_err(|e| {
            let _ = fs::remove_file(&temp_path);
            format!("Failed to write config: {}", e)
        })?;

    // Preserve permissions if existing config has them
    #[cfg(unix)]
    {
        if cfg.exists() {
            if let Ok(meta) = fs::metadata(&cfg) {
                let perms = meta.permissions();
                let _ = fs::set_permissions(&temp_path, perms);
            }
        }
    }

    #[cfg(not(windows))]
    fs::rename(&temp_path, &cfg).map_err(|e| {
        let _ = fs::remove_file(&temp_path);
        format!("Failed to atomically replace config: {}", e)
    })?;

    #[cfg(windows)]
    {
        let old_path = dir.join(format!(".hemp.conf.{}.{:09}.old", timestamp, nanos));
        if cfg.exists() {
            fs::rename(&cfg, &old_path)
                .map_err(|e| format!("Failed to prepare config replacement: {}", e))?;
        }
        if let Err(error) = fs::rename(&temp_path, &cfg) {
            let _ = fs::rename(&old_path, &cfg);
            let _ = fs::remove_file(&temp_path);
            return Err(format!("Failed to replace config: {}", error));
        }
        let _ = fs::remove_file(old_path);
    }

    Ok(())
}

#[tauri::command]
pub fn apply_guided_config(
    changes: HashMap<String, Option<String>>,
    preview_token: String,
) -> Result<ConfigPreview, String> {
    let cfg = config_path()?;
    let original = if cfg.exists() {
        fs::read_to_string(&cfg).map_err(|e| e.to_string())?
    } else {
        String::new()
    };

    // TOCTOU guard: verify the config hasn't changed since preview
    let expected_token = compute_preview_token(&original, &changes);
    if expected_token != preview_token {
        return Err(
            "Config or guided changes were modified since preview. Reload and preview again before applying.".to_string(),
        );
    }

    let applied = apply_config_changes(&original, &changes);
    let new_parsed = parse_config_from_str(&applied).unwrap_or_default();
    let (validation_errors, _) = validate_config_content(&applied);

    if !validation_errors.is_empty() {
        return Err(validation_errors.join("; "));
    }

    let config_changes = detect_config_changes(&original, &changes);
    if config_changes.is_empty() {
        let old_parsed = parse_config_from_str(&original).unwrap_or_default();
        let (reindex, reindex_chainstate) = detect_reindex_requirements(&old_parsed, &new_parsed);
        return Ok(ConfigPreview {
            changes: vec![],
            validation_warnings: vec![],
            validation_errors: vec![],
            reindex_required: reindex,
            reindex_chainstate_required: reindex_chainstate,
            restart_required: false,
            preview_token: expected_token,
        });
    }

    atomic_write_config(&applied)?;

    let old_parsed = parse_config_from_str(&original).unwrap_or_default();
    let (reindex, reindex_chainstate) = detect_reindex_requirements(&old_parsed, &new_parsed);

    let redacted_changes: Vec<ConfigChange> = config_changes
        .iter()
        .map(|c| {
            let mut rc = c.clone();
            if is_secret_key(&c.key) {
                rc.old_value = c.old_value.as_ref().map(|_| "***REDACTED***".to_string());
                rc.new_value = c.new_value.as_ref().map(|_| "***REDACTED***".to_string());
            }
            rc
        })
        .collect();

    Ok(ConfigPreview {
        changes: redacted_changes,
        validation_warnings: vec![],
        validation_errors: vec![],
        reindex_required: reindex,
        reindex_chainstate_required: reindex_chainstate,
        restart_required: !config_changes.is_empty(),
        preview_token: expected_token,
    })
}

#[tauri::command]
pub fn get_config_help_reference() -> Result<Vec<ConfigHelpSection>, String> {
    Ok(vec![
        ConfigHelpSection {
            title: "Essential Settings".to_string(),
            entries: vec![
                ConfigHelpEntry {
                    key: "server".to_string(),
                    description: "Accepts JSON-RPC commands. Required for Commander to control the node.".to_string(),
                    default_value: "1".to_string(),
                    commander_relevance: "Commander requires server=1 for all wallet, asset, and explorer features.".to_string(),
                },
                ConfigHelpEntry {
                    key: "daemon".to_string(),
                    description: "Run in background (1) or interactively (0). Set to 0 on Windows and when using Commander.".to_string(),
                    default_value: "0".to_string(),
                    commander_relevance: "Commander manages the daemon process. daemon=0 is required for Windows and recommended for Commander on all platforms.".to_string(),
                },
                ConfigHelpEntry {
                    key: "listen".to_string(),
                    description: "Accept connections from outside peers. 1 = full node, 0 = leech mode.".to_string(),
                    default_value: "1".to_string(),
                    commander_relevance: "listen=1 helps the network. Set to 0 if you have limited bandwidth.".to_string(),
                },
                ConfigHelpEntry {
                    key: "rpcport".to_string(),
                    description: "Local JSON-RPC port used by Core clients.".to_string(),
                    default_value: "8766 (Core); 42068 (Commander template)".to_string(),
                    commander_relevance: "Commander reads the active Core configuration. Keep RPC bound to localhost unless you have a secured remote-RPC design.".to_string(),
                },
                ConfigHelpEntry {
                    key: "port".to_string(),
                    description: "Peer-to-peer network port used for incoming node connections.".to_string(),
                    default_value: "42069 (mainnet)".to_string(),
                    commander_relevance: "This is not the RPC port. Forward it only when you intentionally accept inbound peers.".to_string(),
                },
            ],
        },
        ConfigHelpSection {
            title: "Performance & Storage".to_string(),
            entries: vec![
                ConfigHelpEntry {
                    key: "dbcache".to_string(),
                    description: "Database cache size in MB. Higher = more RAM, faster sync and rescans. Core minimum is 4.".to_string(),
                    default_value: "450".to_string(),
                    commander_relevance: "Commander's Full Feature preset uses 4096 for fast sync on systems with >=8GB RAM.".to_string(),
                },
                ConfigHelpEntry {
                    key: "prune".to_string(),
                    description: "Reduce storage by deleting old blocks. 0=off, 1=manual (RPC only), >=550=auto-prune to target MiB. Incompatible with txindex and historical rescans. Reverting requires re-downloading the blockchain.".to_string(),
                    default_value: "0".to_string(),
                    commander_relevance: "Full Commander features (explorer, wallet rescan, asset operations) require prune=0. Pruned nodes lose historical scan capability, explorer detail, and full wallet recovery. Setting prune>0 forces txindex=0 and disables historical address lookups.".to_string(),
                },
                ConfigHelpEntry {
                    key: "maxconnections".to_string(),
                    description: "Maximum peer connections. Default 125. Lower if you have limited bandwidth.".to_string(),
                    default_value: "125".to_string(),
                    commander_relevance: "More peers = faster sync and better network health, but more bandwidth use.".to_string(),
                },
                ConfigHelpEntry {
                    key: "maxmempool".to_string(),
                    description: "Maximum mempool memory in MB before lower-fee transactions are evicted.".to_string(),
                    default_value: "Core default".to_string(),
                    commander_relevance: "Increase only when the system has sufficient RAM and transaction volume requires it.".to_string(),
                },
                ConfigHelpEntry {
                    key: "mempoolexpiry / persistmempool".to_string(),
                    description: "Control how long unconfirmed transactions remain and whether the mempool is saved across restarts.".to_string(),
                    default_value: "Core defaults".to_string(),
                    commander_relevance: "Normally leave these unchanged. Disabling persistence can make restart behavior less predictable.".to_string(),
                },
            ],
        },
        ConfigHelpSection {
            title: "Indexes".to_string(),
            entries: vec![
                ConfigHelpEntry {
                    key: "txindex".to_string(),
                    description: "Full transaction index. Required for getrawtransaction. Changing requires -reindex. Incompatible with prune>0.".to_string(),
                    default_value: "0".to_string(),
                    commander_relevance: "Used by Explorer TX detail, raw transaction tools, and consolidation tools.".to_string(),
                },
                ConfigHelpEntry {
                    key: "assetindex".to_string(),
                    description: "Asset ownership index. Changing requires -reindex.".to_string(),
                    default_value: "0".to_string(),
                    commander_relevance: "Used by Asset features, snapshot requests, reward distribution.".to_string(),
                },
                ConfigHelpEntry {
                    key: "addressindex".to_string(),
                    description: "Full address index for balance, txid, and UTXO queries. Changing requires -reindex-chainstate.".to_string(),
                    default_value: "0".to_string(),
                    commander_relevance: "Used by Address lookups, UTXO consolidation, and coin control.".to_string(),
                },
                ConfigHelpEntry {
                    key: "timestampindex".to_string(),
                    description: "Timestamp index for block hashes. Changing requires -reindex-chainstate.".to_string(),
                    default_value: "0".to_string(),
                    commander_relevance: "Used by Transaction history and explorer block lookups.".to_string(),
                },
                ConfigHelpEntry {
                    key: "spentindex".to_string(),
                    description: "Spent output index. Changing requires -reindex-chainstate.".to_string(),
                    default_value: "0".to_string(),
                    commander_relevance: "Used by UTXO management and consolidation tools.".to_string(),
                },
                ConfigHelpEntry {
                    key: "messageindex".to_string(),
                    description: "Index all valid on-chain asset messages so community chat can discover and recover history. Recommended for H0XC chat. Uses local Core data only. 0=off (default), 1=full index. Changing requires a daemon restart.".to_string(),
                    default_value: "0".to_string(),
                    commander_relevance: "Recommended for H0XC community chat. After enabling, run RECOVER H0XC HISTORY (rescanmessages) once to backfill. Historical message recovery is limited on pruned nodes because older blocks may not be available locally.".to_string(),
                },
            ],
        },
        ConfigHelpSection {
            title: "RPC & Authentication".to_string(),
            entries: vec![
                ConfigHelpEntry {
                    key: "rpcuser / rpcpassword".to_string(),
                    description: "Legacy username/password RPC authentication. Core Next uses cookie auth by default (auto-generated .cookie file).".to_string(),
                    default_value: "Not set".to_string(),
                    commander_relevance: "Commander uses cookie auth automatically when available. Never expose rpcpassword in the UI or logs.".to_string(),
                },
                ConfigHelpEntry {
                    key: "rpcallowip".to_string(),
                    description: "IP addresses allowed to issue RPC commands. 127.0.0.1 = localhost only (most secure).".to_string(),
                    default_value: "127.0.0.1 (implicit)".to_string(),
                    commander_relevance: "Only localhost RPC is needed for Commander. Do not expose RPC to remote IPs.".to_string(),
                },
                ConfigHelpEntry {
                    key: "rpcbind".to_string(),
                    description: "Network interface and optional port on which the RPC server listens.".to_string(),
                    default_value: "Loopback".to_string(),
                    commander_relevance: "Keep RPC on loopback for a normal Commander installation. Remote binding requires firewalling and authentication.".to_string(),
                },
                ConfigHelpEntry {
                    key: "rpcthreads / rpcworkqueue".to_string(),
                    description: "Set RPC worker concurrency and queued-request capacity.".to_string(),
                    default_value: "Core defaults".to_string(),
                    commander_relevance: "Advanced tuning only. Excessively low values can make Commander requests stall under load.".to_string(),
                },
                ConfigHelpEntry {
                    key: "rest".to_string(),
                    description: "Enable the public REST interface provided by Core.".to_string(),
                    default_value: "0".to_string(),
                    commander_relevance: "Commander does not require REST. Leave disabled unless another trusted local service needs it.".to_string(),
                },
            ],
        },
        ConfigHelpSection {
            title: "Peer Connections".to_string(),
            entries: vec![
                ConfigHelpEntry {
                    key: "addnode".to_string(),
                    description: "Add a node to connect to. Format IP:port. Repeatable. One entry per line. Supports IPv4, hostname, and bracketed IPv6.".to_string(),
                    default_value: "Not set".to_string(),
                    commander_relevance: "Commander can add bootstrap addnodes. Addnode peers are protected from automatic Peer Guard bans.".to_string(),
                },
                ConfigHelpEntry {
                    key: "connect".to_string(),
                    description: "Connect only to specified node(s). connect=0 disables automatic connections.".to_string(),
                    default_value: "Not set".to_string(),
                    commander_relevance: "Ordinarily not needed. Use addnode for reliable bootstrap peers.".to_string(),
                },
                ConfigHelpEntry {
                    key: "seednode".to_string(),
                    description: "Connect to a node once to discover additional peers. Repeatable.".to_string(),
                    default_value: "Not set".to_string(),
                    commander_relevance: "Useful for bootstrap recovery; addnode is better for peers that should remain preferred.".to_string(),
                },
                ConfigHelpEntry {
                    key: "bind / externalip".to_string(),
                    description: "Choose local listening interfaces and advertise a reachable public address.".to_string(),
                    default_value: "Automatic".to_string(),
                    commander_relevance: "Advanced inbound-node settings. Incorrect values can prevent peer connectivity.".to_string(),
                },
                ConfigHelpEntry {
                    key: "discover / dnsseed".to_string(),
                    description: "Control local-address discovery and DNS-based peer discovery.".to_string(),
                    default_value: "1".to_string(),
                    commander_relevance: "Leave enabled for normal network discovery unless operating a deliberately isolated node.".to_string(),
                },
                ConfigHelpEntry {
                    key: "onlynet / proxy / proxyrandomize".to_string(),
                    description: "Restrict network types and route outbound connections through a proxy.".to_string(),
                    default_value: "Not set".to_string(),
                    commander_relevance: "Use together for Tor or other privacy networks. Test connectivity after changing them.".to_string(),
                },
                ConfigHelpEntry {
                    key: "bantime / banscore".to_string(),
                    description: "Control how long misbehaving peers remain banned and the score that triggers a ban.".to_string(),
                    default_value: "Core defaults".to_string(),
                    commander_relevance: "Peer Guard works with Core's ban system. Avoid aggressive values that can isolate the node.".to_string(),
                },
                ConfigHelpEntry {
                    key: "maxuploadtarget".to_string(),
                    description: "Daily outbound historical-block serving limit in MiB. Zero disables the limit.".to_string(),
                    default_value: "0".to_string(),
                    commander_relevance: "Set a limit when bandwidth is constrained; normal transaction relay continues.".to_string(),
                },
            ],
        },
        ConfigHelpSection {
            title: "Wallet Behavior".to_string(),
            entries: vec![
                ConfigHelpEntry {
                    key: "wallet".to_string(),
                    description: "Select a named Core wallet to load at startup. May be specified more than once when Core supports multiple loaded wallets.".to_string(),
                    default_value: "wallet.dat".to_string(),
                    commander_relevance: "Commander manages this setting when switching between a portable vault wallet and legacy wallet.dat mode.".to_string(),
                },
                ConfigHelpEntry {
                    key: "disablewallet".to_string(),
                    description: "Start Core without wallet functionality.".to_string(),
                    default_value: "0".to_string(),
                    commander_relevance: "Must remain disabled for Commander wallet, send, receive, vault, and solo-mining workflows.".to_string(),
                },
                ConfigHelpEntry {
                    key: "walletbroadcast / walletnotify".to_string(),
                    description: "Control wallet transaction broadcast and optionally run a command when wallet transactions change.".to_string(),
                    default_value: "Core defaults".to_string(),
                    commander_relevance: "Commander does not require walletnotify. Leave wallet broadcast enabled for normal sending.".to_string(),
                },
                ConfigHelpEntry {
                    key: "keypool / fallbackfee / paytxfee / walletrbf".to_string(),
                    description: "Advanced wallet address-pool, fee, and transaction-replacement controls.".to_string(),
                    default_value: "Core defaults".to_string(),
                    commander_relevance: "Change only with a clear fee or wallet-management requirement; unsafe fee values can affect every send.".to_string(),
                },
            ],
        },
        ConfigHelpSection {
            title: "Optional: ZMQ Notifications".to_string(),
            entries: vec![
                ConfigHelpEntry {
                    key: "zmqpubrawtx".to_string(),
                    description: "Publish each raw mempool transaction to local ZMQ subscribers. Example: tcp://127.0.0.1:28332.".to_string(),
                    default_value: "Not set".to_string(),
                    commander_relevance: "Commander does not require it. WebCom ingesters and external indexers may use it. Keep the endpoint local because raw transaction data is exposed to subscribers.".to_string(),
                },
                ConfigHelpEntry {
                    key: "zmqpubhashtx / zmqpubhashblock".to_string(),
                    description: "Publish transaction or block hashes when Core accepts new data.".to_string(),
                    default_value: "Not set".to_string(),
                    commander_relevance: "Useful for lightweight local event consumers that do not need full serialized objects.".to_string(),
                },
                ConfigHelpEntry {
                    key: "zmqpubrawblock".to_string(),
                    description: "Publish full serialized blocks to ZMQ subscribers.".to_string(),
                    default_value: "Not set".to_string(),
                    commander_relevance: "High-volume integration option. Enable only for a trusted local consumer that requires complete blocks.".to_string(),
                },
            ],
        },
        ConfigHelpSection {
            title: "Logging & Diagnostics".to_string(),
            entries: vec![
                ConfigHelpEntry {
                    key: "debug / debugexclude".to_string(),
                    description: "Enable selected debug categories and suppress noisy categories.".to_string(),
                    default_value: "Not set".to_string(),
                    commander_relevance: "Use temporarily when diagnosing a problem; broad debug logging can grow debug.log quickly.".to_string(),
                },
                ConfigHelpEntry {
                    key: "logtimestamps".to_string(),
                    description: "Prefix debug log entries with timestamps.".to_string(),
                    default_value: "1".to_string(),
                    commander_relevance: "Keep enabled so Commander and Core events can be correlated during troubleshooting.".to_string(),
                },
            ],
        },
        ConfigHelpSection {
            title: "Reindex Commands".to_string(),
            entries: vec![
                ConfigHelpEntry {
                    key: "-reindex".to_string(),
                    description: "Rebuild chain state and block index. Required when changing txindex, assetindex, or returning to unpruned mode. Can take hours.".to_string(),
                    default_value: "Not set".to_string(),
                    commander_relevance: "Schedule via System > Repair. Back up wallet.dat first.".to_string(),
                },
                ConfigHelpEntry {
                    key: "-reindex-chainstate".to_string(),
                    description: "Rebuild chain state from existing blocks. Required when changing addressindex, spentindex, or timestampindex. Faster than full reindex.".to_string(),
                    default_value: "Not set".to_string(),
                    commander_relevance: "Schedule via System > Repair. Should complete faster than full reindex.".to_string(),
                },
            ],
        },
    ])
}

fn address_book_path() -> Result<PathBuf, String> {
    Ok(commander_dir()?.join("address_book.json"))
}

fn legacy_address_book_path() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("address_book.json"))
}

fn migrate_legacy_address_book() -> Result<PathBuf, String> {
    let path = address_book_path()?;
    if path.exists() {
        return Ok(path);
    }
    let legacy = legacy_address_book_path()?;
    if legacy.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::copy(&legacy, &path).map_err(|e| format!("Failed to migrate address book: {e}"))?;
    }
    Ok(path)
}

#[tauri::command]
pub fn load_address_book() -> Result<Vec<AddressBookEntry>, String> {
    let path = migrate_legacy_address_book()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let entries: Vec<AddressBookEntry> = serde_json::from_str(&content).unwrap_or_default();
    Ok(entries)
}

#[tauri::command]
pub fn save_address_book(entries: Vec<AddressBookEntry>) -> Result<(), String> {
    let path = address_book_path()?;
    let content = serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())?;
    fs::write(&path, content).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn load_app_settings() -> Result<AppSettings, String> {
    load_app_settings_impl()
}

#[tauri::command]
pub fn save_app_settings(settings: AppSettings) -> Result<(), String> {
    save_app_settings_impl(&settings)
}

#[tauri::command]
pub fn read_text_file(path: String) -> Result<String, String> {
    let resolved = validate_read_path(&path)?;
    validate_path_in_allowed_roots(&resolved)?;
    fs::read_to_string(&resolved).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn write_text_file(path: String, content: String) -> Result<(), String> {
    let resolved = validate_write_path(&path)?;
    validate_path_in_allowed_roots(&resolved)?;
    fs::write(&resolved, content).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn dialog_read_text_file(
    app: AppHandle,
    title: Option<String>,
    filters: Vec<Vec<String>>,
) -> Result<String, String> {
    let mut builder = app.dialog().file();
    if let Some(ref t) = title {
        builder = builder.set_title(t);
    }
    for filter in &filters {
        if filter.len() >= 2 {
            let name = &filter[0];
            let exts: Vec<&str> = filter[1..].iter().map(|s| s.as_str()).collect();
            builder = builder.add_filter(name, &exts);
        }
    }
    let file_path = builder.blocking_pick_file().ok_or("No file selected")?;
    let path = file_path.as_path().ok_or("Invalid file path")?;
    let resolved = validate_read_path(&path.to_string_lossy())?;
    fs::read_to_string(&resolved).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn dialog_write_text_file(
    app: AppHandle,
    content: String,
    default_path: Option<String>,
    title: Option<String>,
    filters: Vec<Vec<String>>,
) -> Result<String, String> {
    let mut builder = app.dialog().file();
    if let Some(ref t) = title {
        builder = builder.set_title(t);
    }
    if let Some(ref dp) = default_path {
        let default = PathBuf::from(dp);
        if default.is_absolute() {
            if let Some(parent) = default.parent() {
                builder = builder.set_directory(parent);
            }
            if let Some(file_name) = default.file_name().and_then(|name| name.to_str()) {
                builder = builder.set_file_name(file_name);
            }
        } else {
            builder = builder.set_file_name(dp);
        }
    }
    for filter in &filters {
        if filter.len() >= 2 {
            let name = &filter[0];
            let exts: Vec<&str> = filter[1..].iter().map(|s| s.as_str()).collect();
            builder = builder.add_filter(name, &exts);
        }
    }
    let file_path = builder.blocking_save_file().ok_or("No file selected")?;
    let path = file_path.as_path().ok_or("Invalid file path")?;
    let resolved = validate_write_path(&path.to_string_lossy())?;
    fs::write(&resolved, content).map_err(|e| e.to_string())?;
    Ok(resolved.to_string_lossy().to_string())
}

fn validate_read_path(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Path is empty".to_string());
    }
    let p = PathBuf::from(trimmed);
    if !p.is_absolute() {
        return Err("Path must be absolute".to_string());
    }
    if !p.exists() {
        return Err("File does not exist".to_string());
    }
    if p.is_dir() {
        return Err("Path is a directory, not a file".to_string());
    }
    let canonical = p
        .canonicalize()
        .map_err(|e| format!("Cannot resolve path: {e}"))?;
    Ok(canonical)
}

fn validate_write_path(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Path is empty".to_string());
    }
    let p = PathBuf::from(trimmed);
    if !p.is_absolute() {
        return Err("Path must be absolute".to_string());
    }
    if p.is_dir() {
        return Err("Path is a directory, not a file".to_string());
    }
    let parent = p.parent().ok_or("Path has no parent directory")?;
    if !parent.exists() {
        return Err("Parent directory does not exist".to_string());
    }
    let canonical_parent = parent
        .canonicalize()
        .map_err(|e| format!("Cannot resolve parent directory: {e}"))?;
    let file_name = p.file_name().ok_or("Path has no filename")?;
    let resolved = canonical_parent.join(file_name);
    Ok(resolved)
}

fn validate_path_in_allowed_roots(path: &Path) -> Result<(), String> {
    let canonical = if path.exists() {
        path.canonicalize()
            .map_err(|e| format!("Cannot resolve path: {e}"))?
    } else {
        let parent = path.parent().ok_or("Path has no parent directory")?;
        let canonical_parent = parent
            .canonicalize()
            .map_err(|e| format!("Cannot resolve parent directory: {e}"))?;
        let file_name = path.file_name().ok_or("Path has no filename")?;
        canonical_parent.join(file_name)
    };

    let data_dir = data_dir()?;
    let data_canonical = data_dir.canonicalize().unwrap_or_else(|_| data_dir.clone());
    if canonical.starts_with(&data_canonical) {
        return Ok(());
    }

    let content_lib_dir = commander_content_library_dir()?;
    let content_lib_canonical = content_lib_dir
        .canonicalize()
        .unwrap_or_else(|_| content_lib_dir.clone());
    if canonical.starts_with(&content_lib_canonical) {
        return Ok(());
    }

    Err(format!(
        "Path is outside allowed directories. Allowed: data directory ({}) and content library.",
        data_dir.to_string_lossy()
    ))
}

#[tauri::command]
pub fn init_config() -> Result<ConfigPaths, String> {
    let cfg = ensure_config()?;
    let dir = data_dir()?;
    let custom_bin_dir = load_app_settings_impl()
        .ok()
        .and_then(|s| s.custom_core_binary_dir);
    let resolve = |name: &str| -> String {
        if let Some(ref d) = custom_bin_dir {
            resolve_bin_with_override(name, Some(d))
        } else {
            resolve_bin(name)
        }
    };
    Ok(ConfigPaths {
        data_dir: dir.to_string_lossy().to_string(),
        config_path: cfg.to_string_lossy().to_string(),
        daemon_path: resolve("hemp0xd"),
        cli_path: resolve("hemp0x-cli"),
    })
}

#[tauri::command]
pub fn get_commander_settings_path() -> Result<String, String> {
    commander_settings_path().map(|p| p.to_string_lossy().to_string())
}

#[tauri::command]
pub fn read_config() -> Result<String, String> {
    let cfg = ensure_config()?;
    fs::read_to_string(cfg).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn write_config(contents: String) -> Result<(), String> {
    if contents.trim().is_empty() {
        return Err("Refusing to write an empty hemp.conf.".to_string());
    }
    atomic_write_config(&contents)
}

#[tauri::command]
pub fn check_config_exists() -> Result<bool, String> {
    let cfg = config_path()?;
    Ok(cfg.exists())
}

#[tauri::command]
pub fn create_default_config() -> Result<(), String> {
    let cfg = config_path()?;
    let dir = data_dir()?;

    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    }

    // SAFETY: Never overwrite an existing `hemp.conf`. The user's config
    // can contain carefully tuned settings (dbcache, indexes, peers, RPC
    // credentials, comments, and the `wallet=` line we use to select the
    // default Core runtime wallet). A silent overwrite has previously
    // broken a user's daemon. The connect flow must never mutate this
    // file. If you really need a fresh default config, the user must
    // explicitly delete the existing one first.
    if cfg.exists() {
        log::warn!(
            "create_default_config refused to overwrite existing hemp.conf at {}",
            cfg.to_string_lossy()
        );
        return Err(format!(
      "hemp.conf already exists at {}; refusing to overwrite it. Delete the existing config file first if you want Commander to write a fresh default.",
      cfg.to_string_lossy()
    ));
    }

    let default_config = r#"# Hemp0x Configuration File
# Core cookie auth is used by default; rpcuser/rpcpassword are not required.
server=1
daemon=0
listen=1
txindex=1
assetindex=1
port=42069
rpcport=42068
"#;

    fs::write(&cfg, default_config).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn get_data_folder_info() -> Result<DataFolderInfo, String> {
    let default_dir = default_core_data_dir()?;
    let settings_path = commander_settings_path().unwrap_or_else(|_| PathBuf::from(""));
    let bootstrap = bootstrap_path().unwrap_or_else(|_| PathBuf::from(""));

    // Try to resolve the active data dir; fall back to default if bootstrap is invalid
    let (dir, bootstrap_error) = match active_data_dir() {
        Ok(d) => (d, None),
        Err(e) => (default_dir.clone(), Some(e)),
    };

    let folder_exists = dir.exists();
    let config_exists = dir.join("hemp.conf").exists();
    let wallet_exists = dir.join("wallet.dat").exists();
    let blocks_exists = dir.join("blocks").exists();
    let chainstate_exists = dir.join("chainstate").exists();
    let debug_log_exists = dir.join("debug.log").exists();
    let lock_exists = dir.join(".lock").exists();

    let size_bytes = if folder_exists {
        calculate_dir_size(&dir)
    } else {
        0
    };
    let size_display = format_size(size_bytes);

    Ok(DataFolderInfo {
        path: dir.to_string_lossy().to_string(),
        default_path: default_dir.to_string_lossy().to_string(),
        using_custom_path: bootstrap_error.is_none() && dir != default_dir,
        commander_settings_path: settings_path.to_string_lossy().to_string(),
        bootstrap_path: bootstrap.to_string_lossy().to_string(),
        size_bytes,
        size_display,
        config_exists,
        wallet_exists,
        folder_exists,
        blocks_exists,
        chainstate_exists,
        debug_log_exists,
        lock_exists,
        bootstrap_error,
    })
}

#[tauri::command]
pub fn open_data_dir() -> Result<(), String> {
    let dir = data_dir()?;

    if cfg!(windows) {
        Command::new("explorer")
            .arg(&dir)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    } else if cfg!(target_os = "macos") {
        Command::new("open")
            .arg(&dir)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    } else {
        // Try dolphin directly (KDE)
        if Command::new("dolphin")
            .arg("--new-window")
            .arg(&dir)
            .spawn()
            .is_ok()
        {
            return Ok(());
        }

        // Try nautilus (GNOME)
        if Command::new("nautilus").arg(&dir).spawn().is_ok() {
            return Ok(());
        }

        Ok(())
    }
}

fn read_log_tail(path: &Path, max_lines: usize) -> Result<String, String> {
    if !path.exists() {
        return Ok(String::from("Log file not found."));
    }
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    let read_size = std::cmp::min(size, 2 * 1024 * 1024);
    file.seek(SeekFrom::End(-(read_size as i64)))
        .map_err(|e| e.to_string())?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = buf.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    Ok(lines[start..].join("\n"))
}

#[tauri::command]
pub fn read_log(lines: Option<u32>) -> Result<String, String> {
    let dir = data_dir()?;
    let log_path = dir.join("debug.log");
    read_log_tail(&log_path, lines.unwrap_or(200) as usize)
}

#[tauri::command]
pub fn truncate_log() -> Result<(), String> {
    let dir = data_dir()?;
    let log_path = dir.join("debug.log");
    if log_path.exists() {
        fs::write(&log_path, "").map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn backup_data_folder() -> Result<String, String> {
    let dir = data_dir()?;
    if !dir.exists() {
        return Err("Data folder does not exist".to_string());
    }

    let ts = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let backup_name = format!("hemp0x_data_backup_{}", ts);

    let backup_base = dirs::desktop_dir()
        .or_else(dirs::home_dir)
        .ok_or("Could not determine backup location")?;
    let backup_path = backup_base.join(&backup_name);

    safe_copy_dir_recursive(&dir, &backup_path)?;
    Ok(backup_path.to_string_lossy().to_string())
}

#[tauri::command]
pub fn backup_data_folder_to(path: String) -> Result<(), String> {
    let dir = data_dir()?;
    if !dir.exists() {
        return Err("Data folder does not exist".to_string());
    }

    safe_copy_dir_recursive(&dir, Path::new(&path))?;
    Ok(())
}

#[tauri::command]
pub fn extract_binaries(target_dir: String) -> Result<String, String> {
    let target_path = PathBuf::from(target_dir);
    if !target_path.exists() {
        return Err("Target directory does not exist".to_string());
    }

    let bins = ["hemp0xd", "hemp0x-cli", "hemp0x-tx"];
    let mut extracted = Vec::new();

    for bin in bins {
        let src_str = resolve_bin(bin);
        let src = PathBuf::from(&src_str);
        if !src.exists() {
            // Try resolving without extension if on Windows and failed? No, resolve_bin handles it.
            return Err(format!("Source binary not found: {}", src_str));
        }

        let dest = target_path.join(bin_name(bin));
        fs::copy(&src, &dest).map_err(|e| format!("Failed to copy {}: {}", bin, e))?;

        // Set executable permissions on Linux/Mac
        #[cfg(unix)]
        {
            if let Ok(metadata) = fs::metadata(&dest) {
                let mut perms = metadata.permissions();
                perms.set_mode(0o755);
                let _ = fs::set_permissions(&dest, perms);
            }
        }
        extracted.push(bin);
    }

    Ok(format!("Successfully extracted: {}", extracted.join(", ")))
}

#[tauri::command]
pub async fn get_binary_status() -> Result<BinaryStatus, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let custom_bin_dir = load_app_settings_impl()
            .ok()
            .and_then(|s| s.custom_core_binary_dir);
        get_binary_status_with_override(custom_bin_dir.as_deref())
    })
    .await
    .map_err(|e| format!("Binary status task failed: {e}"))?
}

// SAFETY: replace_binaries removed in Slice 17d.
// External Core folder selection (non-destructive) is deferred to a future slice.
// See SystemHub.svelte for the placeholder UI.

#[tauri::command]
pub async fn extract_snapshot(archive_path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || extract_snapshot_blocking(archive_path))
        .await
        .map_err(|error| format!("Snapshot extraction task failed: {error}"))?
}

fn extract_snapshot_blocking(archive_path: String) -> Result<String, String> {
    let archive = Path::new(&archive_path);
    if !archive.exists() {
        return Err("Snapshot file not found".to_string());
    }

    let dest_dir = data_dir()?;

    // Create a temp extraction folder
    let temp_extract = dest_dir.join("_snapshot_temp");
    if temp_extract.exists() {
        fs::remove_dir_all(&temp_extract).map_err(|e| format!("Failed to clean temp: {}", e))?;
    }
    fs::create_dir_all(&temp_extract).map_err(|e| format!("Failed to create temp dir: {}", e))?;

    // Extract to temp folder
    sevenz_rust::decompress_file(&archive, &temp_extract)
        .map_err(|e| format!("7z extraction failed: {}", e))?;

    // Find blocks and chainstate folders - check root level first, then one level deep
    fn find_chain_folders(base: &Path) -> Option<(Option<PathBuf>, Option<PathBuf>)> {
        let mut blocks_path: Option<PathBuf> = None;
        let mut chainstate_path: Option<PathBuf> = None;

        // Check root level
        let root_blocks = base.join("blocks");
        let root_chainstate = base.join("chainstate");

        if root_blocks.exists() && root_blocks.is_dir() {
            blocks_path = Some(root_blocks);
        }
        if root_chainstate.exists() && root_chainstate.is_dir() {
            chainstate_path = Some(root_chainstate);
        }

        // If found at root, return
        if blocks_path.is_some() || chainstate_path.is_some() {
            return Some((blocks_path, chainstate_path));
        }

        // Check one level deep (e.g., snapshot_folder/blocks)
        if let Ok(entries) = fs::read_dir(base) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let sub_dir = entry.path();
                    let nested_blocks = sub_dir.join("blocks");
                    let nested_chainstate = sub_dir.join("chainstate");

                    if nested_blocks.exists() && nested_blocks.is_dir() {
                        blocks_path = Some(nested_blocks);
                    }
                    if nested_chainstate.exists() && nested_chainstate.is_dir() {
                        chainstate_path = Some(nested_chainstate);
                    }

                    if blocks_path.is_some() || chainstate_path.is_some() {
                        return Some((blocks_path, chainstate_path));
                    }
                }
            }
        }

        None
    }

    let (blocks_path, chainstate_path) = find_chain_folders(&temp_extract).ok_or_else(|| {
        "Invalid snapshot: could not find 'blocks' or 'chainstate' folder".to_string()
    })?;

    let has_blocks = blocks_path.is_some();
    let has_chainstate = chainstate_path.is_some();

    // Remove existing folders and move new ones
    if let Some(blocks_src) = blocks_path {
        let old_blocks = dest_dir.join("blocks");
        if old_blocks.exists() {
            fs::remove_dir_all(&old_blocks)
                .map_err(|e| format!("Failed to remove old blocks: {}", e))?;
        }
        fs::rename(&blocks_src, &old_blocks)
            .map_err(|e| format!("Failed to install blocks: {}", e))?;
    }

    if let Some(chainstate_src) = chainstate_path {
        let old_chainstate = dest_dir.join("chainstate");
        if old_chainstate.exists() {
            fs::remove_dir_all(&old_chainstate)
                .map_err(|e| format!("Failed to remove old chainstate: {}", e))?;
        }
        fs::rename(&chainstate_src, &old_chainstate)
            .map_err(|e| format!("Failed to install chainstate: {}", e))?;
    }

    // Clean up temp
    let _ = fs::remove_dir_all(&temp_extract);

    let msg = format!(
        "Snapshot installed! blocks: {}, chainstate: {}",
        if has_blocks { "OK" } else { "-" },
        if has_chainstate { "OK" } else { "-" }
    );
    Ok(msg)
}

#[tauri::command]
pub fn set_core_data_dir(path: String) -> Result<DataFolderInfo, String> {
    let p = PathBuf::from(&path);
    if path.trim().is_empty() {
        return Err("Path cannot be empty".to_string());
    }
    if !p.is_absolute() {
        return Err("Path must be absolute".to_string());
    }
    if p.exists() && p.is_file() {
        return Err("Path points to a file, not a directory".to_string());
    }
    if !p.exists() {
        fs::create_dir_all(&p).map_err(|e| e.to_string())?;
    }
    let current_dir = data_dir()?;
    if p == current_dir {
        return get_data_folder_info();
    }

    // Switching the active data dir changes the wallet identity binding, so
    // invalidate the local wallet-PIN record in the OLD dir before switching.
    // Best effort only — never block a data-dir switch over PIN cleanup.
    let _ = crate::modules::wallet_pin_unlock::invalidate_pin_record();

    // 1. Load current settings from wherever they are now
    let mut settings = load_app_settings_impl()?;

    // 2. Write bootstrap pointer to default data dir
    save_bootstrap(Some(path.clone()))?;

    // 3. Update settings for backwards compatibility
    settings.custom_data_dir = Some(path.clone());

    // 4. Now active_data_dir() resolves to the new path — save settings there
    save_app_settings_impl(&settings)?;

    // 5. Ensure Core config exists in the custom data dir
    ensure_config()?;

    get_data_folder_info()
}

#[tauri::command]
pub fn reset_core_data_dir() -> Result<DataFolderInfo, String> {
    // Invalidate the local wallet-PIN record before the data dir resets; the
    // new default dir has a different wallet identity binding. Best effort.
    let _ = crate::modules::wallet_pin_unlock::invalidate_pin_record();
    // 1. Clear bootstrap pointer FIRST so an invalid bootstrap does not block reset
    save_bootstrap(None)?;

    // 2. Load current settings from active location (now default, since bootstrap is cleared)
    let mut settings = load_app_settings_impl().unwrap_or_default();

    // 3. Update settings for backwards compatibility
    settings.custom_data_dir = None;

    // 4. active_data_dir() now resolves to default — save settings there
    save_app_settings_impl(&settings)?;

    // 5. Ensure default data dir exists
    let default_dir = default_core_data_dir()?;
    if !default_dir.exists() {
        fs::create_dir_all(&default_dir).map_err(|e| e.to_string())?;
    }

    // 6. Ensure Core config exists in default dir
    ensure_config()?;

    get_data_folder_info()
}

#[tauri::command]
pub fn prepare_core_data_dir_move(target_path: String) -> Result<DataMovePreview, String> {
    let source = data_dir()?;
    let target = PathBuf::from(&target_path);
    if target_path.trim().is_empty() {
        return Err("Target path cannot be empty".to_string());
    }
    if !target.is_absolute() {
        return Err("Target path must be absolute".to_string());
    }
    if target.exists() && target.is_file() {
        return Err("Target path points to a file, not a directory".to_string());
    }

    if path_contains(&source, &target) {
        return Err("Target directory is inside the source directory".to_string());
    }
    if target.exists() && path_contains(&target, &source) {
        return Err("Source directory is inside the target directory".to_string());
    }
    if let Some(normalized_target) = nearest_existing_absolute_ancestor(&target) {
        if path_contains(&source, &normalized_target) {
            return Err("Target path would be inside the source directory".to_string());
        }
    }

    let source_size = calculate_dir_size(&source);
    let target_exists = target.exists();
    let target_is_empty = target_exists
        && fs::read_dir(&target)
            .map(|mut r| r.next().is_none())
            .unwrap_or(true);
    let target_has_files = target_exists && !target_is_empty;

    let mut warnings: Vec<String> = Vec::new();
    if target_has_files {
        warnings.push(
            "Target directory exists and contains files. Copy will be refused for safety."
                .to_string(),
        );
    }
    if source.join(".lock").exists() {
        warnings.push(
            "Source has a .lock file. The daemon may be running. Stop it before move.".to_string(),
        );
    }

    Ok(DataMovePreview {
        source_path: source.to_string_lossy().to_string(),
        target_path: target.to_string_lossy().to_string(),
        source_size_bytes: source_size,
        source_size_display: format_size(source_size),
        target_exists,
        target_is_empty,
        target_has_files,
        wallet_present: source.join("wallet.dat").exists(),
        config_present: source.join("hemp.conf").exists(),
        blocks_present: source.join("blocks").exists(),
        chainstate_present: source.join("chainstate").exists(),
        warnings,
    })
}

#[tauri::command]
pub fn copy_core_data_dir_to(target_path: String) -> Result<DataMoveResult, String> {
    let source = data_dir()?;
    let target = PathBuf::from(&target_path);
    if !source.exists() {
        return Err("Source data directory does not exist".to_string());
    }
    if source.join(".lock").exists() {
        return Err(
            "Cannot copy while daemon .lock file exists. Stop the daemon first.".to_string(),
        );
    }

    if path_contains(&source, &target) {
        return Err("Target directory is inside the source directory".to_string());
    }
    if target.exists() && path_contains(&target, &source) {
        return Err("Source directory is inside the target directory".to_string());
    }
    if let Some(normalized_target) = nearest_existing_absolute_ancestor(&target) {
        if path_contains(&source, &normalized_target) {
            return Err("Target path would be inside the source directory".to_string());
        }
    }

    if target.exists() {
        let has_content = fs::read_dir(&target)
            .map(|mut r| r.next().is_some())
            .unwrap_or(false);
        if has_content {
            return Err(
                "Target directory is not empty. Clear it or choose a different target.".to_string(),
            );
        }
    }
    if !target.exists() {
        fs::create_dir_all(&target).map_err(|e| e.to_string())?;
    }

    let (file_count, byte_count) = safe_copy_dir_recursive(&source, &target)?;
    Ok(DataMoveResult {
        success: true,
        message: format!(
            "Copied {} files ({}) to {}",
            file_count,
            format_size(byte_count),
            target.to_string_lossy()
        ),
        files_copied: file_count,
        bytes_copied: byte_count,
    })
}

fn safe_absolute_path(p: &Path) -> PathBuf {
    if p.is_absolute() {
        let mut normalized = PathBuf::new();
        for c in p.components() {
            match c {
                std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                    normalized.push(c.as_os_str());
                }
                std::path::Component::Normal(name) => {
                    normalized.push(name);
                }
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
            }
        }
        normalized
    } else {
        p.to_path_buf()
    }
}

fn safe_copy_dir_recursive(src: &Path, dst: &Path) -> Result<(u64, u64), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let mut file_count: u64 = 0;
    let mut byte_count: u64 = 0;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            let (fc, bc) = safe_copy_dir_recursive(&src_path, &dst_path)?;
            file_count += fc;
            byte_count += bc;
        } else {
            let meta = fs::metadata(&src_path).map_err(|e| e.to_string())?;
            fs::copy(&src_path, &dst_path).map_err(|e| e.to_string())?;
            file_count += 1;
            byte_count += meta.len();
        }
    }
    Ok((file_count, byte_count))
}

fn path_contains(a: &Path, b: &Path) -> bool {
    let a_abs = safe_absolute_path(a);
    let b_abs = safe_absolute_path(b);
    if let (Ok(canon_a), Ok(canon_b)) = (a.canonicalize(), b.canonicalize()) {
        return canon_b.starts_with(&canon_a);
    }
    b_abs.starts_with(&a_abs)
}

fn nearest_existing_absolute_ancestor(p: &Path) -> Option<PathBuf> {
    if p.is_absolute() && p.exists() {
        return Some(p.to_path_buf());
    }
    let mut current = p.to_path_buf();
    loop {
        if let Some(parent) = current.parent() {
            if parent.as_os_str().is_empty() {
                return None;
            }
            if !parent.is_absolute() {
                continue;
            }
            if parent.exists() {
                if let Ok(canon) = parent.canonicalize() {
                    let rel = current
                        .strip_prefix(parent)
                        .unwrap_or_else(|_| Path::new(""));
                    return Some(canon.join(rel));
                }
                return Some(current.to_path_buf());
            }
            current = parent.to_path_buf();
        } else {
            return None;
        }
    }
}

#[tauri::command]
pub fn set_daemon_repair_mode(mode: String) -> Result<AppSettings, String> {
    match mode.as_str() {
        "none" | "reindex" | "reindex-chainstate" => {}
        _ => {
            return Err(
                "Invalid repair mode. Use 'none', 'reindex', or 'reindex-chainstate'.".to_string(),
            )
        }
    }
    let mut settings = load_app_settings_impl()?;
    if mode == "none" {
        settings.pending_repair_mode = None;
    } else {
        settings.pending_repair_mode = Some(mode.clone());
        settings.active_repair_mode = Some(mode);
        settings.active_repair_started_at = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        );
    }
    save_app_settings_impl(&settings)?;
    Ok(settings)
}

#[tauri::command]
pub fn clear_daemon_repair_mode() -> Result<AppSettings, String> {
    let mut settings = load_app_settings_impl()?;
    settings.pending_repair_mode = None;
    settings.active_repair_mode = None;
    settings.active_repair_started_at = None;
    save_app_settings_impl(&settings)?;
    Ok(settings)
}

#[tauri::command]
pub fn get_daemon_repair_mode() -> Result<String, String> {
    let settings = load_app_settings_impl()?;
    Ok(settings
        .pending_repair_mode
        .unwrap_or_else(|| "none".to_string()))
}

const REQUIRED_CORE_BINS: &[&str] = &["hemp0xd", "hemp0x-cli"];
const OPTIONAL_CORE_BINS: &[&str] = &["hemp0x-tx"];

#[tauri::command]
pub fn set_core_binary_dir(path: String) -> Result<BinaryStatus, String> {
    let p = PathBuf::from(&path);
    if path.trim().is_empty() {
        return Err("Path cannot be empty".to_string());
    }
    if !p.is_absolute() {
        return Err("Path must be absolute".to_string());
    }
    if !p.exists() {
        return Err("Directory does not exist".to_string());
    }
    if !p.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    for bin in REQUIRED_CORE_BINS {
        let bin_path = p.join(bin_name(bin));
        if !bin_path.exists() {
            return Err(format!(
                "Missing required binary: {} (not found at {})",
                bin,
                bin_path.display()
            ));
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for bin in REQUIRED_CORE_BINS.iter().chain(OPTIONAL_CORE_BINS.iter()) {
            let bin_path = p.join(bin_name(bin));
            if let Ok(meta) = fs::metadata(&bin_path) {
                let mut perms = meta.permissions();
                let mode = perms.mode();
                if mode & 0o111 == 0 {
                    perms.set_mode(0o755);
                    fs::set_permissions(&bin_path, perms).map_err(|e| {
                        format!("Failed to set executable permission on {}: {}", bin, e)
                    })?;
                }
            }
        }
    }

    let mut settings = load_app_settings_impl()?;
    settings.custom_core_binary_dir = Some(path.clone());
    save_app_settings_impl(&settings)?;

    get_binary_status_with_override(Some(&path))
}

#[tauri::command]
pub fn reset_core_binary_dir() -> Result<BinaryStatus, String> {
    let mut settings = load_app_settings_impl()?;
    settings.custom_core_binary_dir = None;
    save_app_settings_impl(&settings)?;
    get_binary_status_with_override(None)
}

#[tauri::command]
pub fn get_core_binary_dir() -> Result<Option<String>, String> {
    let settings = load_app_settings_impl()?;
    Ok(settings.custom_core_binary_dir)
}

fn get_binary_status_with_override(override_dir: Option<&str>) -> Result<BinaryStatus, String> {
    let daemon_path = resolve_bin_with_override("hemp0xd", override_dir);
    let cli_path = resolve_bin_with_override("hemp0x-cli", override_dir);
    let tx_path = resolve_bin_with_override("hemp0x-tx", override_dir);
    Ok(BinaryStatus {
        daemon_exists: PathBuf::from(&daemon_path).exists(),
        cli_exists: PathBuf::from(&cli_path).exists(),
        tx_exists: PathBuf::from(&tx_path).exists(),
        daemon_path,
        cli_path,
        tx_path,
    })
}

fn read_log_recent_lines(path: &Path, max_lines: usize) -> Vec<String> {
    const MAX_LOG_TAIL_BYTES: u64 = 256 * 1024;
    if !path.exists() || max_lines == 0 {
        return Vec::new();
    }
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let file_len = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(_) => return Vec::new(),
    };
    let read_start = file_len.saturating_sub(MAX_LOG_TAIL_BYTES);
    if file.seek(SeekFrom::Start(read_start)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::with_capacity((file_len - read_start) as usize);
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let content = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    if read_start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].to_vec()
}

fn parse_log_hint(lines: &[String]) -> Option<String> {
    for line in lines.iter().rev() {
        let lower = line.to_lowercase();
        if lower.contains("init message: init message channels")
            || lower.contains("scanning asset transactions")
        {
            return Some("Scanning asset transactions".to_string());
        }
        if lower.contains("rescan") || lower.contains("rescanning") {
            if let Some(progress) = extract_progress_percent(&lower) {
                return Some(format!("Rescanning wallet ({progress})"));
            }
            return Some("Rescanning wallet".to_string());
        }
        if lower.contains("loading addresses from dns seeds") {
            return Some("Loading DNS seed peers".to_string());
        }
        if lower.contains("0 addresses found from dns seeds") {
            return Some("DNS seeds returned no peers; waiting for fixed seeds".to_string());
        }
        if lower.contains("adding fixed seed nodes") {
            return Some("Adding fixed seed peers".to_string());
        }
        if lower.contains("invalid or missing peers.dat") {
            return Some("Rebuilding peer database".to_string());
        }
        if lower.contains("timeout downloading block") {
            return Some("Peer block download timed out".to_string());
        }
        if lower.contains("connect() to") && lower.contains("failed") {
            return Some("Trying alternate peers".to_string());
        }
        if lower.contains("reindexing") {
            return Some("Reindexing block data".to_string());
        }
        if lower.contains("loadblockindex") {
            return Some("Loading block index".to_string());
        }
        if lower.contains("loaded block index") {
            return Some("Block index loaded".to_string());
        }
        if lower.contains("verifying blocks") {
            return Some("Verifying chainstate".to_string());
        }
        if lower.contains("updatetip") {
            return Some("RPC online, syncing".to_string());
        }
        if lower.contains("progress=") {
            return Some("Syncing in progress".to_string());
        }
        if lower.contains("shutdown") {
            return Some("Daemon shutting down".to_string());
        }
    }
    None
}

fn extract_progress_percent(line: &str) -> Option<String> {
    let marker = "progress=";
    let start = line.find(marker)? + marker.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let value = rest[..end].parse::<f64>().ok()?;
    if value <= 1.0 {
        Some(format!("{:.2}%", value * 100.0))
    } else {
        Some(format!("{:.2}%", value))
    }
}

#[tauri::command]
pub async fn get_daemon_repair_status() -> Result<RepairStatus, String> {
    tauri::async_runtime::spawn_blocking(get_daemon_repair_status_blocking)
        .await
        .map_err(|error| format!("Repair status task failed: {error}"))?
}

fn get_daemon_repair_status_blocking() -> Result<RepairStatus, String> {
    let mut settings = load_app_settings_impl()?;
    let pending_mode = settings.pending_repair_mode.clone();
    let active_mode = settings.active_repair_mode.clone();
    let dir = data_dir().ok();
    let lock_exists = dir
        .as_ref()
        .map(|d| d.join(".lock").exists())
        .unwrap_or(false);
    let debug_log_path = dir.as_ref().map(|d| d.join("debug.log"));

    let blockchain_info = crate::modules::rpc::rpc_context()
        .ok()
        .and_then(|ctx| ctx.call("getblockchaininfo", &[]).ok());
    let rpc_online = blockchain_info.is_some();
    let (blocks, headers, verification_progress) = match blockchain_info {
        Some(data) => (
            data["blocks"].as_u64(),
            data["headers"].as_u64(),
            data["verificationprogress"].as_f64(),
        ),
        None => (None, None, None),
    };

    let log_lines = debug_log_path
        .as_ref()
        .map(|p| read_log_recent_lines(p, 50))
        .unwrap_or_default();
    let log_hint = parse_log_hint(&log_lines);
    let latest_log_line = log_lines.last().cloned();

    let has_pending = pending_mode.as_deref() == Some("reindex")
        || pending_mode.as_deref() == Some("reindex-chainstate");
    let has_active = active_mode.as_deref() == Some("reindex")
        || active_mode.as_deref() == Some("reindex-chainstate");
    let shutdown_seen = latest_log_line
        .as_ref()
        .map(|line| {
            let lower = line.to_lowercase();
            lower.contains("shutdown")
                || lower.contains("shutting down")
                || lower.contains("scheduler thread interrupt")
        })
        .unwrap_or(false);
    let repair_mode = if has_active {
        active_mode.clone()
    } else {
        pending_mode.clone()
    };

    let repair_work_ongoing = log_hint
        .as_ref()
        .map(|h| {
            let l = h.to_lowercase();
            l.contains("reindexing") || l.contains("loading block index") || l.contains("verifying")
        })
        .unwrap_or(false);
    let sync_ongoing = log_hint
        .as_ref()
        .map(|h| h.to_lowercase().contains("syncing"))
        .unwrap_or(false);

    let chain_catchup_ongoing = match (blocks, headers) {
        (Some(blocks), Some(headers)) => headers > blocks,
        _ => verification_progress
            .map(|progress| progress < 0.999_999)
            .unwrap_or(false),
    };
    let rpc_healthy = rpc_online && blocks.is_some() && headers.is_some();
    let repair_complete = rpc_healthy && !repair_work_ongoing && !chain_catchup_ongoing;

    if repair_complete && has_active {
        settings.active_repair_mode = None;
        settings.active_repair_started_at = None;
        save_app_settings_impl(&settings)?;
    }

    let is_repair_active = has_active || has_pending;
    let is_startup_repair = is_repair_active && !rpc_online;

    let phase = if is_repair_active && shutdown_seen {
        "Stopping daemon; repair will resume on next start".to_string()
    } else if is_startup_repair {
        if lock_exists {
            "Starting daemon with repair flag".to_string()
        } else {
            log_hint
                .clone()
                .unwrap_or_else(|| "Reindexing block data".to_string())
        }
    } else if repair_complete {
        "Repair complete / node online".to_string()
    } else if rpc_online && (repair_work_ongoing || sync_ongoing || chain_catchup_ongoing) {
        "RPC online, syncing".to_string()
    } else if rpc_online && !repair_work_ongoing && has_active {
        "Node online; repair/sync status available".to_string()
    } else if is_repair_active {
        log_hint
            .clone()
            .unwrap_or_else(|| "Daemon starting".to_string())
    } else {
        "Idle".to_string()
    };

    Ok(RepairStatus {
        active: is_repair_active && (!repair_complete || repair_work_ongoing),
        mode: repair_mode,
        phase,
        rpc_online,
        lock_exists,
        blocks,
        headers,
        verification_progress,
        latest_log_line,
        log_hint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn path_contains_target_inside_source() {
        let dir = std::env::temp_dir().join("hemp17d_contains_test_1");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source");
        let target = dir.join("source").join("child");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        assert!(
            path_contains(&source, &target),
            "child should be inside source"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_contains_source_inside_target() {
        let dir = std::env::temp_dir().join("hemp17d_contains_test_2");
        let _ = fs::remove_dir_all(&dir);
        let source = dir.join("child");
        let target = dir;
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        assert!(
            path_contains(&target, &source),
            "source(child) should be inside target(dir)"
        );
        let _ = fs::remove_dir_all(&target);
    }

    #[test]
    fn path_contains_unrelated_paths() {
        let dir = std::env::temp_dir().join("hemp17d_contains_test_3");
        let _ = fs::remove_dir_all(&dir);
        let a = dir.join("a");
        let b = dir.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        assert!(
            !path_contains(&a, &b),
            "sibling dirs should not contain each other"
        );
        assert!(
            !path_contains(&b, &a),
            "sibling dirs should not contain each other"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_contains_non_existing_target() {
        let dir = std::env::temp_dir().join("hemp17d_contains_test_4");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let a = dir.clone();
        let b = dir.join("nonexistent_child");
        assert!(
            path_contains(&a, &b),
            "non-existing child should still be detected as inside parent"
        );
        let _ = fs::remove_dir_all(&a);
    }

    #[test]
    fn validate_read_path_accepts_existing_file() {
        let dir = std::env::temp_dir().join("hemp61b_read_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.txt");
        fs::write(&file, "hello").unwrap();
        let result = validate_read_path(&file.to_string_lossy());
        assert!(result.is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_read_path_rejects_nonexistent() {
        let dir = std::env::temp_dir().join("hemp61b_read_nonexistent");
        let file = dir.join("missing.txt");
        let result = validate_read_path(&file.to_string_lossy());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn validate_read_path_rejects_directory() {
        let dir = std::env::temp_dir().join("hemp61b_read_dir");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let result = validate_read_path(&dir.to_string_lossy());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("directory"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_read_path_rejects_empty() {
        assert!(validate_read_path("").is_err());
        assert!(validate_read_path("   ").is_err());
    }

    #[test]
    fn validate_read_path_rejects_relative() {
        assert!(validate_read_path("relative/path.txt").is_err());
    }

    #[test]
    fn validate_write_path_accepts_new_file_under_existing_parent() {
        let dir = std::env::temp_dir().join("hemp61b_write_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("new_output.txt");
        let result = validate_write_path(&file.to_string_lossy());
        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert_eq!(resolved.file_name().unwrap(), "new_output.txt");
        assert!(resolved.parent().unwrap().exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_write_path_rejects_missing_parent() {
        let dir = std::env::temp_dir().join("hemp61b_write_missing_parent");
        let file = dir.join("subdir").join("output.txt");
        let result = validate_write_path(&file.to_string_lossy());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn validate_write_path_rejects_directory_target() {
        let dir = std::env::temp_dir().join("hemp61b_write_dir_target");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let result = validate_write_path(&dir.to_string_lossy());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("directory"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_write_path_rejects_empty() {
        assert!(validate_write_path("").is_err());
        assert!(validate_write_path("   ").is_err());
    }

    #[test]
    fn validate_write_path_rejects_relative() {
        assert!(validate_write_path("relative/output.txt").is_err());
    }

    #[test]
    fn validate_write_path_resolves_symlink_parent() {
        let dir = std::env::temp_dir().join("hemp61b_symlink_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let real_dir = dir.join("real");
        fs::create_dir_all(&real_dir).unwrap();
        let link_dir = dir.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real_dir, &link_dir).unwrap();
        let file = link_dir.join("output.txt");
        let result = validate_write_path(&file.to_string_lossy());
        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert!(resolved.starts_with(&real_dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_path_in_allowed_roots_accepts_data_dir_child() {
        // Slice 64h: serialize with the new `create_default_config`
        // tests so the global HOME env var is not raced. This test
        // intentionally mutates HOME without restoring; the lock keeps
        // that mutation from interleaving with another HOME-mutating
        // test in this module.
        let _lock = lock_cfg_test();
        let dir = std::env::temp_dir().join("hemp61b_allowed_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let data_dir = dir.join(".hemp0x");
        fs::create_dir_all(&data_dir).unwrap();
        let file = data_dir.join("test.txt");
        fs::write(&file, "hello").unwrap();
        std::env::set_var("HOME", dir.to_str().unwrap());
        let result = validate_path_in_allowed_roots(&file);
        assert!(result.is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_path_in_allowed_roots_rejects_outside_path() {
        let _lock = lock_cfg_test();
        let dir = std::env::temp_dir().join("hemp61b_reject_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let data_dir = dir.join(".hemp0x");
        fs::create_dir_all(&data_dir).unwrap();
        let outside = dir.join("outside").join("test.txt");
        fs::create_dir_all(dir.join("outside")).unwrap();
        fs::write(&outside, "hello").unwrap();
        std::env::set_var("HOME", dir.to_str().unwrap());
        let result = validate_path_in_allowed_roots(&outside);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside allowed"));
        let _ = fs::remove_dir_all(&dir);
    }

    // ─── Slice 64h: config overwrite guard tests ────────────────────────
    //
    // `create_default_config` MUST refuse to overwrite an existing
    // `hemp.conf`. A previous version of the function silently
    // overwrote the user's config, which once broke a daemon because
    // the user's tuned settings (dbcache, peer list, RPC credentials,
    // `wallet=` default) were clobbered. The connect flow now treats
    // `hemp.conf` as a user-owned file it must never mutate.

    // Process-wide serialization for tests that mutate HOME + the
    // commander bootstrap pointer. The Rust stdlib explicitly
    // documents `set_var` as not thread-safe, and our `data_dir()`
    // resolution depends on HOME + `<default_core_data_dir>/commander/bootstrap.json`.
    // The pre-existing `validate_path_in_allowed_roots_*` tests above
    // also mutate HOME without restoring it; if my new slice-64h tests
    // race with them, both can read the wrong HOME at the wrong time
    // and produce intermittent failures. We share the same lock so
    // every HOME-mutating test in this module is serialized through
    // one queue.
    static CFG_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static CFG_TEST_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    struct HomeGuard {
        target: PathBuf,
        previous: Option<String>,
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            // Always restore HOME and clean up, even on panic.
            match self.previous.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            if let Some(parent) = self.target.parent() {
                let _ = fs::remove_dir_all(parent);
            }
        }
    }

    fn isolate_data_dir_for_config_test() -> HomeGuard {
        // We use the bootstrap pointer mechanism (the same one
        // `set_core_data_dir` uses) to redirect `data_dir()` to a
        // per-test directory. `set_var("HOME", ...)` alone is not enough
        // because `data_dir()` reads `<default_core_data_dir>/commander/bootstrap.json`
        // first.
        let pid = std::process::id();
        let n = CFG_TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let unique = format!("hemp64h_cfg_p{pid}_n{n}_{nanos}");
        let home_root = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&home_root);
        fs::create_dir_all(&home_root).unwrap();
        // `default_core_data_dir()` returns `<HOME>/.hemp0x` on Linux.
        let default_data = home_root.join(".hemp0x");
        let commander = default_data.join("commander");
        fs::create_dir_all(&commander).unwrap();
        // The custom target data dir for THIS test, with a unique subdir
        // name to avoid races when many tests in the same binary mutate
        // HOME + bootstrap.
        let target = home_root.join(format!("target_data_{pid}_{n}"));
        fs::create_dir_all(&target).unwrap();
        let bootstrap =
            serde_json::json!({ "custom_data_dir": target.to_string_lossy().to_string() });
        fs::write(
            commander.join("bootstrap.json"),
            serde_json::to_string_pretty(&bootstrap).unwrap(),
        )
        .unwrap();

        let previous_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", home_root.to_str().unwrap());
        HomeGuard {
            target,
            previous: previous_home,
        }
    }

    fn lock_cfg_test() -> std::sync::MutexGuard<'static, ()> {
        // Recover from poisoning so a panic in one test thread does not
        // cascade into another.
        match CFG_TEST_LOCK.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    #[test]
    fn create_default_config_refuses_to_overwrite_existing_config() {
        let _lock = lock_cfg_test();
        let guard = isolate_data_dir_for_config_test();
        let cfg = guard.target.join("hemp.conf");
        let original =
            "server=1\ndaemon=1\nrpcport=42068\n# user comment\nwallet=hemp0x-vault-main\n";
        fs::write(&cfg, original).unwrap();

        let result = create_default_config();
        assert!(
            result.is_err(),
            "create_default_config must refuse to overwrite an existing hemp.conf"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("already exists"),
            "error must mention the file exists, got: {err}"
        );
        assert!(
            err.contains("refusing to overwrite"),
            "error must explicitly refuse, got: {err}"
        );

        // The existing config must be byte-for-byte unchanged.
        let after = fs::read_to_string(&cfg).unwrap();
        assert_eq!(
            after, original,
            "hemp.conf was mutated by create_default_config"
        );
        drop(guard);
    }

    #[test]
    fn create_default_config_writes_when_no_existing_config() {
        let _lock = lock_cfg_test();
        let guard = isolate_data_dir_for_config_test();
        let cfg = guard.target.join("hemp.conf");
        assert!(
            !cfg.exists(),
            "test setup: target should not have a hemp.conf yet"
        );

        let result = create_default_config();
        assert!(
            result.is_ok(),
            "create_default_config should succeed when no config exists: {:?}",
            result
        );
        assert!(cfg.exists(), "hemp.conf should be created");
        let content = fs::read_to_string(&cfg).unwrap();
        assert!(content.contains("server=1"));
        assert!(content.contains("rpcport=42068"));
        drop(guard);
    }

    // ─── Slice 64n: hemp.conf safety tests ──────────────────────────────
    //
    // `ensure_config()` MUST refuse to silently create a default
    // `hemp.conf` inside an established Core data directory (one that
    // already has Core state such as blocks/chainstate/wallet.dat). A
    // previous version wrote a bare default config whenever the file was
    // missing, which reduced a user's tuned config to basic settings and
    // lost their custom options during vault import/connect/start.
    #[test]
    fn ensure_config_refuses_to_create_default_in_established_data_dir() {
        let _lock = lock_cfg_test();
        let guard = isolate_data_dir_for_config_test();
        let cfg = guard.target.join("hemp.conf");
        // Simulate an established Core data dir: no hemp.conf, but Core
        // state is present (blocks + chainstate + wallet.dat).
        fs::create_dir_all(guard.target.join("blocks")).unwrap();
        fs::create_dir_all(guard.target.join("chainstate")).unwrap();
        fs::write(guard.target.join("wallet.dat"), b"not-a-real-wallet").unwrap();
        assert!(!cfg.exists(), "test setup: hemp.conf should not exist yet");

        let result = ensure_config();
        assert!(
            result.is_err(),
            "ensure_config must refuse to create a default hemp.conf in an established data dir"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("refused to create a replacement default"),
            "error must explain the refusal, got: {err}"
        );
        // Critical: no hemp.conf was written behind the user's back.
        assert!(
            !cfg.exists(),
            "ensure_config must NOT write a hemp.conf when refusing"
        );
        drop(guard);
    }

    #[test]
    fn ensure_config_creates_default_in_brand_new_empty_data_dir() {
        let _lock = lock_cfg_test();
        let guard = isolate_data_dir_for_config_test();
        let cfg = guard.target.join("hemp.conf");
        // Brand-new/empty data dir: no Core state, no hemp.conf. Creating
        // a default here is safe and expected.
        assert!(!cfg.exists());

        let result = ensure_config();
        assert!(
            result.is_ok(),
            "ensure_config should create a default in an empty data dir: {:?}",
            result
        );
        assert!(
            cfg.exists(),
            "hemp.conf should be created in an empty data dir"
        );
        drop(guard);
    }

    #[test]
    fn ensure_config_creates_default_when_only_commander_dir_exists() {
        let _lock = lock_cfg_test();
        let guard = isolate_data_dir_for_config_test();
        let cfg = guard.target.join("hemp.conf");

        // Commander settings are app-owned metadata, not Core state. A
        // fresh custom data dir can legitimately contain only this folder
        // because set_core_data_dir saves app settings before ensuring the
        // Core config. That must not make ensure_config refuse first-run
        // default creation.
        fs::create_dir_all(guard.target.join("commander")).unwrap();
        assert!(!cfg.exists());

        let result = ensure_config();
        assert!(
      result.is_ok(),
      "ensure_config should create a default when only app-owned commander metadata exists: {:?}",
      result
    );
        assert!(cfg.exists(), "hemp.conf should be created");
        drop(guard);
    }

    #[test]
    fn write_config_rejects_empty_content() {
        let _lock = lock_cfg_test();
        let guard = isolate_data_dir_for_config_test();
        let cfg = guard.target.join("hemp.conf");
        let original = "server=1\ndaemon=1\n# tuned by user\ndbcache=2048\n";
        fs::write(&cfg, original).unwrap();

        // Empty / whitespace-only content must be rejected so a buggy
        // caller can never wipe a tuned config to nothing.
        let res = write_config(String::new());
        assert!(res.is_err(), "write_config must reject empty content");
        assert!(
            res.unwrap_err().contains("empty"),
            "error must mention empty content"
        );

        let res_ws = write_config("   \n  \t ".to_string());
        assert!(
            res_ws.is_err(),
            "write_config must reject whitespace-only content"
        );

        // The existing tuned config must be byte-for-byte unchanged.
        let after = fs::read_to_string(&cfg).unwrap();
        assert_eq!(
            after, original,
            "write_config must not mutate hemp.conf when rejecting"
        );
        drop(guard);
    }

    #[test]
    fn tokenize_config_preserves_comments() {
        let content = "# top comment\nserver=1\n# inline comment\nport=42069\n# trailing comment";
        let lines = tokenize_config(content);
        assert_eq!(
            lines.len(),
            5,
            "should preserve all lines including comments"
        );
        assert!(matches!(lines[0], ConfigLine::Comment { .. }));
        assert!(matches!(lines[1], ConfigLine::Option { .. }));
        assert!(matches!(lines[2], ConfigLine::Comment { .. }));
        assert!(matches!(lines[3], ConfigLine::Option { .. }));
        assert!(matches!(lines[4], ConfigLine::Comment { .. }));
    }

    #[test]
    fn tokenize_config_preserves_blank_lines() {
        let content = "server=1\n\n\nport=42069\n";
        let lines = tokenize_config(content);
        assert_eq!(lines.len(), 4, "should preserve blank lines");
        assert!(matches!(lines[1], ConfigLine::Blank { .. }));
        assert!(matches!(lines[2], ConfigLine::Blank { .. }));
    }

    #[test]
    fn tokenize_config_preserves_unknown_options() {
        let content = "server=1\ncustom_option=custom_value\n# comment\nunknown_flag=42\n";
        let lines = tokenize_config(content);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn tokenize_config_preserves_crlf() {
        let content = "# header\r\nserver=1\r\ndaemon=0\r\n# footer\r\n";
        let lines = tokenize_config(content);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn tokenize_config_preserves_lf_without_trailing_newline() {
        let content = "# header\nserver=1\ndaemon=0";
        let lines = tokenize_config(content);
        assert_eq!(lines.len(), 3);
        let reconstructed = reconstruct_config(&lines);
        assert!(
            !reconstructed.ends_with('\n'),
            "should not add trailing newline when absent"
        );
    }

    #[test]
    fn tokenize_config_preserves_leading_whitespace() {
        let content = "  server=1\n\t# indented comment\n  \tunknown line\n";
        let lines = tokenize_config(content);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn tokenize_config_preserves_inline_comments_on_option_lines() {
        let content = "server=1  # RPC server\nport=42069  # main port\n";
        let lines = tokenize_config(content);
        assert_eq!(lines.len(), 2);
        // The raw line text should preserve the full line including inline comment
        if let ConfigLine::Option { raw, .. } = &lines[0] {
            assert!(
                raw.contains("# RPC server"),
                "should preserve inline comment"
            );
        } else {
            panic!("expected option line");
        }
    }

    #[test]
    fn tokenize_config_handles_sectioned_config() {
        let content = "# === NETWORK ===\nserver=1\nport=42069\n# === INDEXES ===\ntxindex=1\n";
        let lines = tokenize_config(content);
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn apply_changes_updates_existing_keys() {
        let original = "# header\nserver=1\ndaemon=0\n# footer\n";
        let mut changes = HashMap::new();
        changes.insert("server".to_string(), Some("0".to_string()));
        let result = apply_config_changes(original, &changes);
        assert!(result.contains("server=0"), "should update server to 0");
        assert!(result.contains("daemon=0"), "should preserve daemon");
        assert!(
            result.contains("# header"),
            "should preserve header comment"
        );
        assert!(
            result.contains("# footer"),
            "should preserve footer comment"
        );
    }

    #[test]
    fn apply_changes_adds_absent_keys() {
        let original = "# header\nserver=1\n";
        let mut changes = HashMap::new();
        changes.insert("txindex".to_string(), Some("1".to_string()));
        let result = apply_config_changes(original, &changes);
        assert!(result.contains("txindex=1"), "should add new key");
        assert!(result.contains("server=1"), "should preserve existing key");
        assert!(result.contains("# header"), "should preserve comment");
    }

    #[test]
    fn apply_changes_adds_messageindex() {
        let original = "# header\nserver=1\n";
        let mut changes = HashMap::new();
        changes.insert("messageindex".to_string(), Some("1".to_string()));
        let result = apply_config_changes(original, &changes);
        assert!(
            result.contains("messageindex=1"),
            "messageindex should be written when enabled from guided config"
        );
    }

    #[test]
    fn apply_changes_does_not_duplicate_keys() {
        let original = "server=1\ntxindex=0\n";
        let mut changes = HashMap::new();
        changes.insert("txindex".to_string(), Some("1".to_string()));
        let result = apply_config_changes(original, &changes);
        let txindex_count = result
            .lines()
            .filter(|l| l.trim().starts_with("txindex="))
            .count();
        assert_eq!(txindex_count, 1, "should not duplicate txindex line");
    }

    #[test]
    fn apply_changes_preserves_line_endings_and_trailing_newline() {
        let original = "server=1\n";
        let mut changes = HashMap::new();
        changes.insert("daemon".to_string(), Some("0".to_string()));
        let result = apply_config_changes(original, &changes);
        assert!(result.ends_with('\n'), "should preserve trailing newline");
    }

    #[test]
    fn apply_changes_preserves_crlf_endings() {
        let original = "# header\r\nserver=1\r\ndaemon=0\r\n";
        let mut changes = HashMap::new();
        changes.insert("server".to_string(), Some("0".to_string()));
        let result = apply_config_changes(original, &changes);
        assert!(
            result.contains("server=0\r\n"),
            "should preserve CRLF line endings"
        );
    }

    #[test]
    fn apply_changes_preserves_addnode_as_repeated_lines() {
        let original = "# peers\naddnode=host1:42069\naddnode=host2:42069\nserver=1\n";
        // Don't modify addnode — should preserve repeated entries
        let mut changes = HashMap::new();
        changes.insert("server".to_string(), Some("0".to_string()));
        let result = apply_config_changes(original, &changes);
        let count = result
            .lines()
            .filter(|l| l.trim().starts_with("addnode="))
            .count();
        assert_eq!(count, 2, "should preserve both addnode entries");
    }

    #[test]
    fn apply_changes_addnode_add_and_remove() {
        let original = "addnode=host1:42069\naddnode=host2:42069\nserver=1\n";
        let mut changes = HashMap::new();
        changes.insert(
            "addnode".to_string(),
            Some("host1:42069,host3:42069".to_string()),
        );
        let result = apply_config_changes(original, &changes);
        assert!(
            result.contains("addnode=host1:42069"),
            "should preserve host1"
        );
        assert!(result.contains("addnode=host3:42069"), "should add host3");
        assert!(!result.contains("host2:42069"), "should remove host2");
        let count = result
            .lines()
            .filter(|l| l.trim().starts_with("addnode="))
            .count();
        assert_eq!(count, 2, "should have exactly 2 addnode lines");
    }

    #[test]
    fn apply_changes_preserves_ipv6_addnode() {
        let original = "addnode=[::1]:42069\nserver=1\n";
        let mut changes = HashMap::new();
        changes.insert("server".to_string(), Some("0".to_string()));
        let result = apply_config_changes(original, &changes);
        assert!(
            result.contains("[::1]:42069"),
            "should preserve IPv6 addnode"
        );
    }

    #[test]
    fn reconstruct_roundtrips_lf() {
        let original = "# config\nserver=1\n\nport=42069\n# end\n";
        let lines = tokenize_config(original);
        let reconstructed = reconstruct_config(&lines);
        assert_eq!(
            reconstructed, original,
            "LF roundtrip should match original"
        );
    }

    #[test]
    fn reconstruct_roundtrips_crlf() {
        let original = "# config\r\nserver=1\r\nport=42069\r\n";
        let lines = tokenize_config(original);
        let reconstructed = reconstruct_config(&lines);
        assert_eq!(
            reconstructed, original,
            "CRLF roundtrip should match original"
        );
    }

    #[test]
    fn apply_changes_preserves_secret_valued_lines() {
        let original = "# do not edit\nrpcpassword=super_secret_value\nserver=1\n";
        let mut changes = HashMap::new();
        changes.insert("server".to_string(), Some("0".to_string()));
        let result = apply_config_changes(original, &changes);
        assert!(
            result.contains("rpcpassword=super_secret_value"),
            "must preserve rpcpassword unchanged"
        );
        assert!(result.contains("server=0"), "should still update server");
    }

    #[test]
    fn apply_changes_preserves_inline_comment_on_changed_line() {
        let original = "server = 1  # Commander RPC\n";
        let mut changes = HashMap::new();
        changes.insert("server".to_string(), Some("0".to_string()));
        assert_eq!(
            apply_config_changes(original, &changes),
            "server = 0  # Commander RPC\n"
        );
    }

    #[test]
    fn config_parsing_ignores_inline_comments() {
        let parsed = parse_config_from_str("server = 1  # Commander RPC\n").unwrap();
        assert_eq!(parsed.get("server").map(String::as_str), Some("1"));
    }

    #[test]
    fn apply_changes_removes_singleton_option() {
        let original = "server=1\nzmqpubrawtx=tcp://127.0.0.1:28332\n";
        let mut changes = HashMap::new();
        changes.insert("zmqpubrawtx".to_string(), None);
        assert_eq!(apply_config_changes(original, &changes), "server=1\n");
    }

    #[test]
    fn duplicate_singleton_conflicts_are_rejected() {
        let errors = duplicate_singleton_errors("server=1\nserver=0\n");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("Conflicting duplicate 'server'"));
    }

    #[test]
    fn sectioned_configs_are_rejected_by_guided_validation() {
        let (errors, _) = validate_config_content("server=1\n[test]\nserver=0\n");
        assert!(errors
            .iter()
            .any(|error| error.contains("network sections")));
    }

    #[test]
    fn validate_rejects_prune_with_txindex_as_error() {
        let mut settings = HashMap::new();
        settings.insert("prune".to_string(), "1000".to_string());
        settings.insert("txindex".to_string(), "1".to_string());
        let (errors, _warnings) = validate_config_settings(&settings);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("incompatible") && e.contains("txindex")),
            "must error about prune+txindex conflict"
        );
    }

    #[test]
    fn validate_rejects_prune_below_minimum_as_error() {
        let mut settings = HashMap::new();
        settings.insert("prune".to_string(), "100".to_string());
        let (errors, _) = validate_config_settings(&settings);
        assert!(
            errors.iter().any(|e| e.contains("550")),
            "must error about prune below 550 minimum"
        );
    }

    #[test]
    fn validate_accepts_prune_1_as_valid() {
        let mut settings = HashMap::new();
        settings.insert("prune".to_string(), "1".to_string());
        settings.insert("txindex".to_string(), "0".to_string());
        let (errors, _) = validate_config_settings(&settings);
        assert!(
            errors.is_empty(),
            "prune=1 should be valid manual-pruning mode"
        );
    }

    #[test]
    fn validate_accepts_valid_prune_alone() {
        let mut settings = HashMap::new();
        settings.insert("prune".to_string(), "2000".to_string());
        settings.insert("txindex".to_string(), "0".to_string());
        let (errors, _warnings) = validate_config_settings(&settings);
        assert!(
            errors.is_empty(),
            "valid prune config should have no errors"
        );
    }

    #[test]
    fn validate_accepts_valid_full_node_config() {
        let mut settings = HashMap::new();
        settings.insert("prune".to_string(), "0".to_string());
        settings.insert("txindex".to_string(), "1".to_string());
        settings.insert("addressindex".to_string(), "1".to_string());
        settings.insert("assetindex".to_string(), "1".to_string());
        settings.insert("spentindex".to_string(), "1".to_string());
        settings.insert("timestampindex".to_string(), "1".to_string());
        let (errors, _) = validate_config_settings(&settings);
        assert!(errors.is_empty(), "full node config should have no errors");
    }

    #[test]
    fn validate_rejects_remote_or_malformed_zmq_endpoints() {
        for endpoint in [
            "tcp://192.168.1.10:28332",
            "tcp://127.0.0.1:",
            "tcp://localhost:0",
            "ipc://",
        ] {
            let mut settings = HashMap::new();
            settings.insert("zmqpubrawtx".to_string(), endpoint.to_string());
            let (errors, _) = validate_config_settings(&settings);
            assert!(
                !errors.is_empty(),
                "endpoint should be rejected by guided configuration: {endpoint}"
            );
        }
    }

    #[test]
    fn validate_accepts_local_zmq_endpoints() {
        for endpoint in [
            "tcp://127.0.0.1:28332",
            "tcp://localhost:28332",
            "tcp://[::1]:28332",
            "ipc:///tmp/hemp0x-zmq",
        ] {
            let mut settings = HashMap::new();
            settings.insert("zmqpubrawtx".to_string(), endpoint.to_string());
            let (errors, _) = validate_config_settings(&settings);
            assert!(
                errors.is_empty(),
                "endpoint should be accepted by guided configuration: {endpoint}"
            );
        }
    }

    #[test]
    fn validate_rejects_invalid_dbcache() {
        let mut settings = HashMap::new();
        settings.insert("dbcache".to_string(), "0".to_string());
        let (errors, _) = validate_config_settings(&settings);
        assert!(!errors.is_empty(), "dbcache=0 should be rejected");
    }

    #[test]
    fn validate_rejects_invalid_maxconnections() {
        let mut settings = HashMap::new();
        settings.insert("maxconnections".to_string(), "0".to_string());
        let (errors, _) = validate_config_settings(&settings);
        assert!(!errors.is_empty(), "maxconnections=0 should be rejected");
    }

    #[test]
    fn validate_rejects_malformed_addnode() {
        let mut settings = HashMap::new();
        settings.insert("addnode".to_string(), "badhost".to_string());
        let (errors, _) = validate_config_settings(&settings);
        assert!(!errors.is_empty(), "malformed addnode should be rejected");
    }

    #[test]
    fn validate_accepts_valid_hostname_addnode() {
        let mut settings = HashMap::new();
        settings.insert("addnode".to_string(), "seed.hemp0x.com:42069".to_string());
        let (errors, _) = validate_config_settings(&settings);
        assert!(errors.is_empty(), "hostname:port addnode should be valid");
    }

    #[test]
    fn validate_accepts_valid_ipv6_addnode() {
        let mut settings = HashMap::new();
        settings.insert("addnode".to_string(), "[2001:db8::1]:42069".to_string());
        let (errors, _) = validate_config_settings(&settings);
        assert!(errors.is_empty(), "IPv6 addnode should be valid");
    }

    #[test]
    fn parse_log_hint_detects_snapshot_message_scan() {
        let lines = vec![
            "2026-06-25 init message: Init Message Channels - Scanning Asset Transactions"
                .to_string(),
        ];
        assert_eq!(
            parse_log_hint(&lines),
            Some("Scanning asset transactions".to_string())
        );
    }

    #[test]
    fn parse_log_hint_detects_rescan_progress() {
        let lines = vec![
            "2026-06-25 init message: Rescanning wallet progress=0.421".to_string(),
        ];
        assert_eq!(
            parse_log_hint(&lines),
            Some("Rescanning wallet (42.10%)".to_string())
        );
    }

    #[test]
    fn parse_log_hint_detects_p2p_block_timeout() {
        let lines = vec![
            "2026-06-25 Timeout downloading block 000000abc from peer=2, disconnecting"
                .to_string(),
        ];
        assert_eq!(
            parse_log_hint(&lines),
            Some("Peer block download timed out".to_string())
        );
    }

    #[test]
    fn detect_reindex_txindex_change() {
        let mut old = HashMap::new();
        old.insert("txindex".to_string(), "0".to_string());
        let mut new = HashMap::new();
        new.insert("txindex".to_string(), "1".to_string());
        let (full, _chainstate) = detect_reindex_requirements(&old, &new);
        assert!(!full.is_empty(), "txindex change requires full reindex");
        assert!(
            full.iter().any(|s| s.contains("-reindex")),
            "should mention -reindex"
        );
    }

    #[test]
    fn detect_reindex_addressindex_change() {
        let mut old = HashMap::new();
        old.insert("addressindex".to_string(), "0".to_string());
        let mut new = HashMap::new();
        new.insert("addressindex".to_string(), "1".to_string());
        let (_full, chainstate) = detect_reindex_requirements(&old, &new);
        assert!(
            !chainstate.is_empty(),
            "addressindex change requires reindex-chainstate"
        );
        assert!(
            chainstate.iter().any(|s| s.contains("-reindex-chainstate")),
            "should mention -reindex-chainstate"
        );
    }

    #[test]
    fn detect_reindex_assetindex_change() {
        let mut old = HashMap::new();
        old.insert("assetindex".to_string(), "0".to_string());
        let mut new = HashMap::new();
        new.insert("assetindex".to_string(), "1".to_string());
        let (full, _chainstate) = detect_reindex_requirements(&old, &new);
        assert!(!full.is_empty(), "assetindex change requires full reindex");
    }

    #[test]
    fn detect_no_reindex_for_non_index_changes() {
        let mut old = HashMap::new();
        old.insert("txindex".to_string(), "1".to_string());
        old.insert("addressindex".to_string(), "1".to_string());
        old.insert("server".to_string(), "0".to_string());
        let mut new = HashMap::new();
        new.insert("txindex".to_string(), "1".to_string());
        new.insert("addressindex".to_string(), "1".to_string());
        new.insert("server".to_string(), "1".to_string());
        let (full, chainstate) = detect_reindex_requirements(&old, &new);
        assert!(full.is_empty(), "no index change, no full reindex needed");
        assert!(
            chainstate.is_empty(),
            "no index change, no chainstate reindex needed"
        );
    }

    #[test]
    fn preview_config_does_not_write() {
        let _lock = lock_cfg_test();
        let guard = isolate_data_dir_for_config_test();
        let cfg = guard.target.join("hemp.conf");
        let original = "# test config\nserver=1\ndaemon=0\n";
        fs::write(&cfg, original).unwrap();

        let mut changes = HashMap::new();
        changes.insert("server".to_string(), Some("0".to_string()));
        let preview = preview_config_changes(changes).unwrap();

        assert!(!preview.changes.is_empty(), "should detect change");
        assert!(preview.restart_required, "server change requires restart");
        assert!(
            !preview.preview_token.is_empty(),
            "should have preview token"
        );

        let on_disk = fs::read_to_string(&cfg).unwrap();
        assert_eq!(on_disk, original, "preview must not write to disk");
        drop(guard);
    }

    #[test]
    fn preview_does_not_return_unrelated_secret_values() {
        let _lock = lock_cfg_test();
        let guard = isolate_data_dir_for_config_test();
        let cfg = guard.target.join("hemp.conf");
        let original = "# header\nserver=1\nrpcpassword=mysecret\nrpcuser=admin\n";
        fs::write(&cfg, original).unwrap();

        let mut changes = HashMap::new();
        changes.insert("server".to_string(), Some("0".to_string()));
        let preview = preview_config_changes(changes).unwrap();

        let serialized = serde_json::to_string(&preview).unwrap();
        assert!(!serialized.contains("mysecret"));
        assert!(!serialized.contains("admin"));
        drop(guard);
    }

    #[test]
    fn apply_guided_config_rejects_changed_request_after_preview() {
        let _lock = lock_cfg_test();
        let guard = isolate_data_dir_for_config_test();
        let cfg = guard.target.join("hemp.conf");
        fs::write(&cfg, "server=1\n").unwrap();

        let mut preview_changes = HashMap::new();
        preview_changes.insert("server".to_string(), Some("0".to_string()));
        let preview = preview_config_changes(preview_changes).unwrap();

        let mut different_changes = HashMap::new();
        different_changes.insert("listen".to_string(), Some("0".to_string()));
        let result = apply_guided_config(different_changes, preview.preview_token);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("guided changes were modified"));
        assert_eq!(fs::read_to_string(&cfg).unwrap(), "server=1\n");
        drop(guard);
    }

    #[test]
    fn apply_guided_config_creates_backup() {
        let _lock = lock_cfg_test();
        let guard = isolate_data_dir_for_config_test();
        let cfg = guard.target.join("hemp.conf");
        let original = "# test\nserver=1\n";
        fs::write(&cfg, original).unwrap();

        let preview = preview_config_changes({
            let mut m = HashMap::new();
            m.insert("server".to_string(), Some("0".to_string()));
            m
        })
        .unwrap();

        let result = apply_guided_config(
            {
                let mut m = HashMap::new();
                m.insert("server".to_string(), Some("0".to_string()));
                m
            },
            preview.preview_token.clone(),
        );

        assert!(result.is_ok(), "apply should succeed: {:?}", result.err());

        // Verify .bak file exists
        let bak_exists = cfg
            .parent()
            .unwrap()
            .read_dir()
            .unwrap()
            .any(|e| e.unwrap().file_name().to_string_lossy().contains(".bak"));
        assert!(bak_exists, "backup file should exist after apply");

        let on_disk = fs::read_to_string(&cfg).unwrap();
        assert!(on_disk.contains("server=0"), "config should be updated");
        drop(guard);
    }

    #[test]
    fn apply_guided_config_writes_messageindex() {
        let _lock = lock_cfg_test();
        let guard = isolate_data_dir_for_config_test();
        let cfg = guard.target.join("hemp.conf");
        fs::write(&cfg, "# test\nserver=1\n").unwrap();

        let mut changes = HashMap::new();
        changes.insert("messageindex".to_string(), Some("1".to_string()));
        let preview = preview_config_changes(changes.clone()).unwrap();

        assert!(
            preview
                .changes
                .iter()
                .any(|change| change.key == "messageindex" && change.new_value.as_deref() == Some("1")),
            "preview should report messageindex change"
        );

        let result = apply_guided_config(changes, preview.preview_token.clone());
        assert!(result.is_ok(), "apply should succeed: {:?}", result.err());

        let on_disk = fs::read_to_string(&cfg).unwrap();
        assert!(
            on_disk.contains("messageindex=1"),
            "guided apply should write messageindex to hemp.conf"
        );
        drop(guard);
    }

    #[test]
    fn apply_guided_config_rejects_stale_preview_token() {
        let _lock = lock_cfg_test();
        let guard = isolate_data_dir_for_config_test();
        let cfg = guard.target.join("hemp.conf");
        fs::write(&cfg, "# original\nserver=1\n").unwrap();

        // Preview
        let preview = preview_config_changes({
            let mut m = HashMap::new();
            m.insert("server".to_string(), Some("0".to_string()));
            m
        })
        .unwrap();

        // Mutate config between preview and apply
        fs::write(&cfg, "# modified externally\nserver=1\ntxindex=1\n").unwrap();

        let result = apply_guided_config(
            {
                let mut m = HashMap::new();
                m.insert("server".to_string(), Some("0".to_string()));
                m
            },
            preview.preview_token.clone(),
        );

        assert!(result.is_err(), "should reject stale preview token");
        assert!(
            result.unwrap_err().contains("modified since preview"),
            "should mention TOCTOU"
        );
        drop(guard);
    }
}
