use std::fs;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::modules::files::load_app_settings_impl;
use crate::modules::rpc::rpc_context;
use crate::modules::rpc::RpcContext;
use crate::modules::utils::{hide_console_window, resolve_bin, resolve_bin_with_override};

const REQUIRED_CORE_NEXT_COMMIT: &str = "3e061497a";
const REQUIRED_CORE_BASE_VERSION: &str = "4.8.1.0";
const DEFAULT_RPC_PORT: u16 = 42068;
const DEFAULT_P2P_PORT: u16 = 42069;

static DAEMON_OWNERSHIP: Mutex<bool> = Mutex::new(false);

#[derive(Serialize)]
pub struct BinaryVersion {
    pub path: String,
    pub exists: bool,
    pub raw: String,
    pub base_version: Option<String>,
    pub commit_hash: Option<String>,
    pub exact_core_next_match: bool,
}

#[derive(Clone, Serialize)]
pub struct DaemonProbe {
    pub rpc_port_open: bool,
    pub p2p_port_open: bool,
    pub default_rpc_port: u16,
    pub default_p2p_port: u16,
}

#[derive(Serialize)]
pub struct RuntimeStatus {
    pub required_base_version: String,
    pub required_commit_hash: String,
    pub daemon: BinaryVersion,
    pub cli: BinaryVersion,
    pub bundled_core_next_ready: bool,
    pub probe: DaemonProbe,
}

#[derive(Serialize)]
pub struct DaemonOwnership {
    pub commander_owns: bool,
}

#[derive(Clone, Serialize)]
pub struct DaemonProcessIdentity {
    pub available: bool,
    pub pid: Option<u32>,
    pub exe_path: Option<String>,
    pub matches_bundled_path: bool,
    pub exe_sha256: Option<String>,
    pub bundled_sha256: Option<String>,
    pub sha256_match: bool,
    pub version_raw: Option<String>,
    pub version_commit_match: bool,
    pub confidence: String,
}

impl DaemonProcessIdentity {
    fn unavailable() -> Self {
        Self {
            available: false,
            pid: None,
            exe_path: None,
            matches_bundled_path: false,
            exe_sha256: None,
            bundled_sha256: None,
            sha256_match: false,
            version_raw: None,
            version_commit_match: false,
            confidence: "none".to_string(),
        }
    }

    pub fn is_exact_bundled_match(&self) -> bool {
        self.available
            && self.confidence == "exact"
            && self.sha256_match
            && self.version_commit_match
    }

    pub fn can_prove_bundled_daemon(&self) -> bool {
        self.available && self.sha256_match && self.version_commit_match
    }
}

#[allow(dead_code)]
fn bundled_daemon_path() -> PathBuf {
    let custom_bin_dir = load_app_settings_impl()
        .ok()
        .and_then(|s| s.custom_core_binary_dir);
    let path = if let Some(ref d) = custom_bin_dir {
        resolve_bin_with_override("hemp0xd", Some(d))
    } else {
        resolve_bin("hemp0xd")
    };
    PathBuf::from(path)
}

#[cfg(target_os = "linux")]
fn find_pid_by_port(port: u16) -> Option<u32> {
    let port_hex = format!(":{:04X}", port);

    let tcp_content = fs::read_to_string("/proc/net/tcp").ok()?;
    let mut socket_inode = None;
    for line in tcp_content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 2 && fields[1].ends_with(&port_hex) {
            socket_inode = fields.get(9).map(|s| s.to_string());
            break;
        }
    }
    let socket_inode = socket_inode?;
    let socket_target = format!("socket:[{}]", socket_inode);

    if let Ok(proc_entries) = fs::read_dir("/proc") {
        for entry in proc_entries.flatten() {
            let pid_dir = entry.path();
            let pid_str = entry.file_name().to_string_lossy().to_string();
            if !pid_str.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let fd_dir = pid_dir.join("fd");
            if !fd_dir.is_dir() {
                continue;
            }
            if let Ok(fd_entries) = fs::read_dir(&fd_dir) {
                for fd_entry in fd_entries.flatten() {
                    if let Ok(link_target) = fs::read_link(fd_entry.path()) {
                        if link_target.to_string_lossy() == socket_target {
                            return pid_str.parse().ok();
                        }
                    }
                }
            }
        }
    }

    None
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
fn find_pid_by_port(_port: u16) -> Option<u32> {
    None
}

#[allow(dead_code)]
fn compute_sha256(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path).map_err(|e| format!("SHA256 open error: {e}"))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| format!("SHA256 read error: {e}"))?;
    Ok(format!("{:x}", hasher.finalize()))
}

#[tauri::command]
pub fn get_daemon_process_identity() -> DaemonProcessIdentity {
    #[cfg(not(target_os = "linux"))]
    {
        return DaemonProcessIdentity::unavailable();
    }

    #[cfg(target_os = "linux")]
    {
        let pid = match find_pid_by_port(DEFAULT_RPC_PORT) {
            Some(p) => p,
            None => return DaemonProcessIdentity::unavailable(),
        };

        let exe_path = match fs::read_link(format!("/proc/{}/exe", pid)) {
            Ok(p) => p,
            Err(_) => return DaemonProcessIdentity::unavailable(),
        };
        let exe_path_str = exe_path.to_string_lossy().to_string();

        let bundled = bundled_daemon_path();
        let matches_bundled_path = exe_path == bundled;

        let exe_sha256 = compute_sha256(&exe_path).ok();
        let bundled_sha256 = if bundled.exists() {
            compute_sha256(&bundled).ok()
        } else {
            None
        };
        let sha256_match = match (&exe_sha256, &bundled_sha256) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };

        let should_run_version_probe = matches_bundled_path || sha256_match;
        let version_raw = if should_run_version_probe {
            command_version(&exe_path_str).ok()
        } else {
            None
        };
        let version_commit_match = version_raw.as_ref().map_or(false, |v| {
            parse_commit_hash(v).as_deref() == Some(REQUIRED_CORE_NEXT_COMMIT)
        });

        let confidence = if matches_bundled_path && sha256_match && version_commit_match {
            "exact"
        } else if sha256_match && version_commit_match {
            "high"
        } else if version_commit_match && matches_bundled_path {
            "medium"
        } else if version_commit_match || matches_bundled_path {
            "low"
        } else {
            "none"
        };

        DaemonProcessIdentity {
            available: true,
            pid: Some(pid),
            exe_path: Some(exe_path_str),
            matches_bundled_path,
            exe_sha256,
            bundled_sha256,
            sha256_match,
            version_raw,
            version_commit_match,
            confidence: confidence.to_string(),
        }
    }
}

fn command_version(path: &str) -> Result<String, String> {
    let mut cmd = Command::new(path);
    hide_console_window(&mut cmd);
    let output = cmd
        .arg("-version")
        .output()
        .map_err(|e| e.to_string())?;

    let mut text = String::new();
    if !output.stdout.is_empty() {
        text.push_str(&String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }

    if text.trim().is_empty() {
        Err("Version command returned no output".to_string())
    } else {
        Ok(text.trim().to_string())
    }
}

fn parse_base_version(raw: &str) -> Option<String> {
    for token in raw.split_whitespace() {
        let trimmed =
            token.trim_matches(|c: char| c == 'v' || c == 'V' || c == ',' || c == ')' || c == '(');
        if trimmed.chars().filter(|c| *c == '.').count() >= 2
            && trimmed.chars().all(|c| c.is_ascii_digit() || c == '.')
        {
            return Some(trimmed.to_string());
        }
        if let Some((front, _)) = trimmed.split_once('-') {
            if front.chars().filter(|c| *c == '.').count() >= 2
                && front.chars().all(|c| c.is_ascii_digit() || c == '.')
            {
                return Some(front.to_string());
            }
        }
    }
    None
}

fn parse_commit_hash(raw: &str) -> Option<String> {
    let lower = raw.to_lowercase();

    for token in lower.split(|c: char| !c.is_ascii_hexdigit()) {
        if token.len() >= 8 && token.len() <= 40 && token.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(token.to_string());
        }
    }
    None
}

fn binary_version(name: &str) -> BinaryVersion {
    let custom_bin_dir = load_app_settings_impl()
        .ok()
        .and_then(|s| s.custom_core_binary_dir);
    let path = if let Some(ref d) = custom_bin_dir {
        resolve_bin_with_override(name, Some(d))
    } else {
        resolve_bin(name)
    };
    let exists = PathBuf::from(&path).exists();
    let raw = if exists {
        command_version(&path).unwrap_or_else(|e| format!("Version check failed: {e}"))
    } else {
        String::new()
    };
    let base_version = parse_base_version(&raw);
    let commit_hash = parse_commit_hash(&raw);
    let exact_core_next_match = base_version.as_deref() == Some(REQUIRED_CORE_BASE_VERSION)
        && commit_hash.as_deref() == Some(REQUIRED_CORE_NEXT_COMMIT);

    BinaryVersion {
        path,
        exists,
        raw,
        base_version,
        commit_hash,
        exact_core_next_match,
    }
}

fn port_open(port: u16) -> bool {
    let addr = format!("127.0.0.1:{port}");
    let Ok(addrs) = addr.to_socket_addrs() else {
        return false;
    };
    for addr in addrs {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok() {
            return true;
        }
    }
    false
}

#[tauri::command]
pub fn probe_default_daemon() -> DaemonProbe {
    DaemonProbe {
        rpc_port_open: port_open(DEFAULT_RPC_PORT),
        p2p_port_open: port_open(DEFAULT_P2P_PORT),
        default_rpc_port: DEFAULT_RPC_PORT,
        default_p2p_port: DEFAULT_P2P_PORT,
    }
}

#[tauri::command]
pub async fn get_runtime_status() -> RuntimeStatus {
    tauri::async_runtime::spawn_blocking(get_runtime_status_blocking)
        .await
        .unwrap_or_else(|_| runtime_status_unavailable())
}

fn runtime_status_unavailable() -> RuntimeStatus {
    RuntimeStatus {
        required_base_version: REQUIRED_CORE_BASE_VERSION.to_string(),
        required_commit_hash: REQUIRED_CORE_NEXT_COMMIT.to_string(),
        daemon: BinaryVersion {
            path: resolve_bin("hemp0xd"),
            exists: false,
            raw: "Runtime status check failed.".to_string(),
            base_version: None,
            commit_hash: None,
            exact_core_next_match: false,
        },
        cli: BinaryVersion {
            path: resolve_bin("hemp0x-cli"),
            exists: false,
            raw: "Runtime status check failed.".to_string(),
            base_version: None,
            commit_hash: None,
            exact_core_next_match: false,
        },
        bundled_core_next_ready: false,
        probe: probe_default_daemon(),
    }
}

fn get_runtime_status_blocking() -> RuntimeStatus {
    let daemon = binary_version("hemp0xd");
    let cli = binary_version("hemp0x-cli");
    let bundled_core_next_ready = daemon.exact_core_next_match && cli.exact_core_next_match;

    RuntimeStatus {
        required_base_version: REQUIRED_CORE_BASE_VERSION.to_string(),
        required_commit_hash: REQUIRED_CORE_NEXT_COMMIT.to_string(),
        daemon,
        cli,
        bundled_core_next_ready,
        probe: probe_default_daemon(),
    }
}

#[tauri::command]
pub fn take_daemon_ownership() -> DaemonOwnership {
    if let Ok(mut guard) = DAEMON_OWNERSHIP.lock() {
        *guard = true;
    }
    DaemonOwnership {
        commander_owns: true,
    }
}

#[tauri::command]
pub fn release_daemon_ownership() -> DaemonOwnership {
    if let Ok(mut guard) = DAEMON_OWNERSHIP.lock() {
        *guard = false;
    }
    DaemonOwnership {
        commander_owns: false,
    }
}

#[tauri::command]
pub fn get_daemon_ownership() -> DaemonOwnership {
    let owns = DAEMON_OWNERSHIP.lock().map(|g| *g).unwrap_or(false);
    DaemonOwnership {
        commander_owns: owns,
    }
}

#[tauri::command]
pub fn exit_commander(app: tauri::AppHandle) {
    crate::modules::process::cleanup_stale_runtime_files();
    app.exit(0);
}

#[derive(Clone, Serialize)]
pub struct RunningDaemonIdentity {
    pub rpc_authenticated: bool,
    pub base_version: Option<String>,
    pub subversion: Option<String>,
    pub build: Option<String>,
    pub build_commit: Option<String>,
    pub protocol_version: Option<u64>,
    pub numeric_version: Option<u64>,
    pub is_required_core_next: bool,
    pub commit_match: bool,
    pub commit_available: bool,
    pub status: String,
    pub capabilities: CoreNextCapabilities,
}

#[derive(Clone, Serialize)]
pub struct CoreNextCapabilities {
    pub help_probe_success: bool,
    pub wallet_migration: bool,
    pub messaging: bool,
    pub restricted_assets: bool,
    pub qualifiers: bool,
    pub rewards: bool,
    pub snapshots: bool,
    pub has_view_channel_messages: bool,
    pub has_message_txid_lookup: bool,
    pub has_message_index: bool,
    pub has_rescan_messages: bool,
    pub detected_rpc_names: Vec<String>,
}

impl Default for CoreNextCapabilities {
    fn default() -> Self {
        Self {
            help_probe_success: false,
            wallet_migration: false,
            messaging: false,
            restricted_assets: false,
            qualifiers: false,
            rewards: false,
            snapshots: false,
            has_view_channel_messages: false,
            has_message_txid_lookup: false,
            has_message_index: false,
            has_rescan_messages: false,
            detected_rpc_names: Vec::new(),
        }
    }
}

pub(crate) fn parse_capabilities_from_help(help_text: &str) -> CoreNextCapabilities {
    if help_text.trim().is_empty() {
        return CoreNextCapabilities::default();
    }

    let lower = help_text.to_lowercase();

    let wallet_migration = lower.contains("exportwalletmigration")
        && lower.contains("validatewalletmigration")
        && lower.contains("restorewalletmigration");

    let messaging = lower.contains("viewallmessages") && lower.contains("viewallmessagechannels");

    let restricted_assets =
        lower.contains("listrestrictedassets") && lower.contains("issuerestrictedasset");

    let qualifiers = lower.contains("listqualifiers") && lower.contains("issuequalifierasset");

    let rewards = lower.contains("distributereward") && lower.contains("getdistributestatus");

    let snapshots = lower.contains("requestsnapshot") && lower.contains("getsnapshot");

    let has_view_channel_messages = lower.contains("viewchannelmessages");
    let has_message_txid_lookup = lower.contains("getmessagetxid");

    // Message index (Core Next v4.8.0.0+): full message index + recovery RPCs.
    let has_message_index = lower.contains("viewindexedmessagechannels");
    let has_rescan_messages = lower.contains("rescanmessages");

    let rpc_names: Vec<String> = lower
        .lines()
        .filter(|line| {
            line.contains("exportwalletmigration")
                || line.contains("validatewalletmigration")
                || line.contains("restorewalletmigration")
                || line.contains("getmessaginginfo")
                || line.contains("viewallmessages")
                || line.contains("viewallmessagechannels")
                || line.contains("viewchannelmessages")
                || line.contains("getmessagetxid")
                || line.contains("viewindexedmessagechannels")
                || line.contains("rescanmessages")
                || line.contains("listrestrictedassets")
                || line.contains("issuerestrictedasset")
                || line.contains("listqualifiers")
                || line.contains("issuequalifierasset")
                || line.contains("distributereward")
                || line.contains("getdistributestatus")
                || line.contains("requestsnapshot")
                || line.contains("getsnapshot")
        })
        .filter_map(|line| line.split_whitespace().next())
        .filter(|w| w.len() > 2 && !w.contains("->"))
        .map(|w| w.to_string())
        .collect();

    CoreNextCapabilities {
        help_probe_success: true,
        wallet_migration,
        messaging,
        restricted_assets,
        qualifiers,
        rewards,
        snapshots,
        has_view_channel_messages,
        has_message_txid_lookup,
        has_message_index,
        has_rescan_messages,
        detected_rpc_names: rpc_names,
    }
}

fn probe_capabilities(ctx: &RpcContext) -> CoreNextCapabilities {
    let help_str = match ctx.call("help", &[]) {
        Ok(data) => data.as_str().unwrap_or("").to_string(),
        Err(_) => return CoreNextCapabilities::default(),
    };

    parse_capabilities_from_help(&help_str)
}

fn parse_numeric_version(raw: u64) -> String {
    let major = raw / 1000000;
    let minor = (raw / 10000) % 100;
    let revision = (raw / 100) % 100;
    let build = raw % 100;
    format!("{major}.{minor}.{revision}.{build}")
}

#[tauri::command]
pub fn identify_running_daemon(allow_non_bundled: Option<bool>) -> RunningDaemonIdentity {
    let allow_override = allow_non_bundled.unwrap_or(false);
    let ctx = match rpc_context() {
        Ok(c) => c,
        Err(e) => {
            return RunningDaemonIdentity {
                rpc_authenticated: false,
                base_version: None,
                subversion: None,
                build: None,
                build_commit: None,
                protocol_version: None,
                numeric_version: None,
                is_required_core_next: false,
                commit_match: false,
                commit_available: false,
                status: format!("RPC not configured: {e}"),
                capabilities: CoreNextCapabilities::default(),
            };
        }
    };

    match ctx.call("getnetworkinfo", &[]) {
        Ok(data) => {
            let subver_str = data["subversion"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let build_str = data["build"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let build_commit_str = data["build_commit"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let numeric_version = data["version"].as_u64();

            let base_version = numeric_version.map(parse_numeric_version);
            let is_required = base_version.as_deref() == Some(REQUIRED_CORE_BASE_VERSION);

            let effective_commit = if !build_commit_str.is_empty() {
                build_commit_str.clone()
            } else {
                subver_str.clone()
            };
            let commit_match = effective_commit.contains(REQUIRED_CORE_NEXT_COMMIT);
            let commit_available = !build_commit_str.is_empty()
                || parse_commit_hash(&subver_str).is_some();
            let is_exact = is_required && numeric_version.is_some() && commit_match;

            let capabilities = probe_capabilities(&ctx);

            let status = if is_exact {
                "A verified Core Next daemon is already running.".to_string()
            } else if allow_override {
                format!(
                    "A daemon is running (non-bundled override active). Version: {} / Build: {}",
                    base_version.as_deref().unwrap_or("?"),
                    if build_str.is_empty() { subver_str.as_str() } else { build_str.as_str() }
                )
            } else if is_required && commit_available {
                format!(
                    "A daemon is running, but it does not match the bundled Core Next build ({}).",
                    REQUIRED_CORE_NEXT_COMMIT,
                )
            } else if is_required && !commit_available && capabilities.help_probe_success {
                "Commander can continue, but cannot prove this daemon is the bundled build because this Core RPC does not expose the build commit.".to_string()
            } else if is_required {
                format!(
                    "A daemon is running with the required base version, but Core RPC did not expose the commit hash needed to verify bundled build {}.",
                    REQUIRED_CORE_NEXT_COMMIT,
                )
            } else {
                format!(
                    "A daemon is running, but Commander could not verify it is Core Next {} ({}).",
                    REQUIRED_CORE_BASE_VERSION,
                    REQUIRED_CORE_NEXT_COMMIT,
                )
            };

            RunningDaemonIdentity {
                rpc_authenticated: true,
                base_version,
                subversion: if subver_str.is_empty() { None } else { Some(subver_str) },
                build: if build_str.is_empty() { None } else { Some(build_str) },
                build_commit: if build_commit_str.is_empty() { None } else { Some(build_commit_str) },
                protocol_version: data["protocolversion"].as_u64(),
                numeric_version,
                is_required_core_next: is_exact || (allow_override && is_required),
                commit_match,
                commit_available,
                status,
                capabilities,
            }
        }
        Err(e) => RunningDaemonIdentity {
            rpc_authenticated: false,
            base_version: None,
            subversion: None,
            build: None,
            build_commit: None,
            protocol_version: None,
            numeric_version: None,
            is_required_core_next: false,
            commit_match: false,
            commit_available: false,
            status: format!(
                "A daemon is listening on the default RPC port, but Commander could not verify its version: {e}"
            ),
            capabilities: CoreNextCapabilities::default(),
        },
    }
}

#[derive(Clone, Serialize)]
pub struct DaemonReadiness {
    pub ready: bool,
    pub progress: String,
    pub elapsed_ms: u64,
    pub retries: u32,
    pub rpc_error: String,
}

#[tauri::command]
pub async fn wait_for_daemon_ready(timeout_ms: Option<u64>) -> Result<DaemonReadiness, String> {
    // Polling loop with sleeps — run off the main thread so the UI does
    // not freeze while waiting for Core RPC to come up.
    tauri::async_runtime::spawn_blocking(move || Ok(wait_for_daemon_ready_blocking(timeout_ms)))
        .await
        .map_err(|e| format!("Wait-for-daemon task failed: {e}"))?
}

pub fn wait_for_daemon_ready_blocking(timeout_ms: Option<u64>) -> DaemonReadiness {
    let timeout_dur = Duration::from_millis(timeout_ms.unwrap_or(30_000));
    let poll_interval = Duration::from_millis(500);
    let start = Instant::now();
    let mut retries: u32 = 0;

    loop {
        retries += 1;

        let ctx = match rpc_context() {
            Ok(c) => c,
            Err(e) => {
                let elapsed = start.elapsed();
                if elapsed >= timeout_dur {
                    return DaemonReadiness {
                        ready: false,
                        progress: "RPC configuration failed repeatedly".to_string(),
                        elapsed_ms: elapsed.as_millis() as u64,
                        retries,
                        rpc_error: e,
                    };
                }
                if poll_interval > timeout_dur.saturating_sub(elapsed) {
                    std::thread::sleep(timeout_dur.saturating_sub(elapsed));
                } else {
                    std::thread::sleep(poll_interval);
                }
                continue;
            }
        };

        match ctx.call("getnetworkinfo", &[]) {
            Ok(data) => {
                let _ = data;
                return DaemonReadiness {
                    ready: true,
                    progress: "Daemon RPC is responding".to_string(),
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    retries,
                    rpc_error: String::new(),
                };
            }
            Err(e) => {
                let elapsed = start.elapsed();
                if elapsed >= timeout_dur {
                    return DaemonReadiness {
                        ready: false,
                        progress: "Daemon did not become ready within timeout".to_string(),
                        elapsed_ms: elapsed.as_millis() as u64,
                        retries,
                        rpc_error: e,
                    };
                }
                if poll_interval > timeout_dur.saturating_sub(elapsed) {
                    std::thread::sleep(timeout_dur.saturating_sub(elapsed));
                } else {
                    std::thread::sleep(poll_interval);
                }
                continue;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_base_version_from_typical_output() {
        let raw = "Hemp0x Core Daemon version v4.8.0.0-192c6b5ce";
        let result = parse_base_version(raw);
        assert_eq!(result, Some("4.8.0.0".to_string()));
    }

    #[test]
    fn parses_base_version_without_v_prefix() {
        let raw = "Hemp0x Daemon 4.7.0-unk";
        let result = parse_base_version(raw);
        assert_eq!(result, Some("4.7.0".to_string()));
    }

    #[test]
    fn parses_base_version_from_alpha() {
        let raw = "Hemp0x RPC client version v4.8.0.0-alpha";
        assert_eq!(parse_base_version(raw), Some("4.8.0.0".to_string()));
    }

    #[test]
    fn parse_base_version_no_version() {
        assert_eq!(parse_base_version("Hemp0x no version here"), None);
    }

    #[test]
    fn parse_base_version_not_enough_dots() {
        let raw = "version 4.7 something";
        assert_eq!(parse_base_version(raw), None);
    }

    #[test]
    fn parse_base_version_two_digits() {
        let raw = "v4.70.0.5 (release)";
        assert_eq!(parse_base_version(raw), Some("4.70.0.5".to_string()));
    }

    #[test]
    fn parses_commit_hash_from_known_hash() {
        let raw = "Hemp0x Core Daemon version v4.8.0.0-192c6b5ce";
        let result = parse_commit_hash(raw);
        assert_eq!(result, Some("192c6b5ce".to_string()));
    }

    #[test]
    fn parses_commit_hash_from_arbitrary_hex() {
        let raw = "Hemp0x Daemon v1.2.3-abc123def456 (release build)";
        let result = parse_commit_hash(raw);
        assert!(result.is_some());
        assert!(result.unwrap().len() >= 8);
    }

    #[test]
    fn parse_commit_hash_short_hex_not_returned() {
        let raw = "Hemp0x Core v4.8.0.0-abc";
        assert_eq!(parse_commit_hash(raw), None);
    }

    #[test]
    fn parse_commit_hash_no_hash() {
        let raw = "Hemp0x Core v4.8.0.0";
        assert_eq!(parse_commit_hash(raw), None);
    }

    #[test]
    fn parse_numeric_version_standard() {
        assert_eq!(parse_numeric_version(4080000), "4.8.0.0");
    }

    #[test]
    fn parse_numeric_version_nonzero_build() {
        assert_eq!(parse_numeric_version(4070001), "4.7.0.1");
    }

    #[test]
    fn parse_numeric_version_minimal() {
        assert_eq!(parse_numeric_version(1), "0.0.0.1");
    }

    #[test]
    fn probe_default_daemon_returns_expected_structure() {
        let probe = probe_default_daemon();
        assert_eq!(probe.default_rpc_port, 42068);
        assert_eq!(probe.default_p2p_port, 42069);
    }

    #[test]
    fn daemon_ownership_cycle() {
        assert_eq!(get_daemon_ownership().commander_owns, false);
        let taken = take_daemon_ownership();
        assert!(taken.commander_owns);
        assert!(get_daemon_ownership().commander_owns);
        let released = release_daemon_ownership();
        assert!(!released.commander_owns);
        assert!(!get_daemon_ownership().commander_owns);
    }

    #[test]
    fn wait_for_daemon_ready_short_timeout_is_non_blocking() {
        let start = std::time::Instant::now();
        let result = wait_for_daemon_ready_blocking(Some(100));
        let elapsed = start.elapsed().as_millis();
        assert!(
            !result.ready || elapsed < 500,
            "if daemon is running, response should be sub-second"
        );
        assert!(elapsed < 5000, "function should not block for long");
    }

    fn synthetic_help() -> &'static str {
        "== Blockchain ==
getbestblockhash
getblock \"headerhash\" ( verbosity )
getblockchaininfo
getblockcount
== Control ==
help ( \"command\" )
stop

== Wallet Migration ==
exportwalletmigration \"filename\"
validatewalletmigration \"filename\"
restorewalletmigration \"filename\" ( \"walletname\" )

== Messages ==
getmessaginginfo
viewallmessages ( count \"asset_name\" \"address\" )
viewallmessagechannels
viewchannelmessages \"channel\" ( count ) ( offset ) ( start_height ) ( stop_height )
getmessagetxid \"channel\" timestamp \"message_hash\"
viewindexedmessagechannels ( \"pattern\" ) ( count ) ( offset )
rescanmessages ( start_height ) ( stop_height ) ( \"channel\" )

== Restricted Assets ==
listrestrictedassets ( \"asset_name\" )
issuerestrictedasset \"asset_name\" \"qty\" \"address\" \"verifier\"
listqualifiers ( \"asset_name\" )
issuequalifierasset \"asset_name\" \"qualifier_name\" \"qty\" (\"destination\" )

== Rewards ==
distributereward \"asset_name\" \"ownership_asset\" snapshot_height distribution_asset gross_amount (exception_addresses)
getdistributestatus \"distributionid\"

== Snapshots ==
requestsnapshot \"asset_name\" ( block_height )
getsnapshot \"asset_name\"

== Generating ==
generate nblocks ( maxtries )
generatetoaddress nblocks address (maxtries)
"
    }

    #[test]
    fn detects_wallet_migration() {
        let caps = parse_capabilities_from_help(synthetic_help());
        assert!(caps.help_probe_success);
        assert!(caps.wallet_migration);
    }

    #[test]
    fn detects_messaging() {
        let caps = parse_capabilities_from_help(synthetic_help());
        assert!(caps.help_probe_success);
        assert!(caps.messaging);
    }

    #[test]
    fn detects_message_index_capabilities() {
        let caps = parse_capabilities_from_help(synthetic_help());
        assert!(caps.has_message_index);
        assert!(caps.has_rescan_messages);
        assert!(caps.has_view_channel_messages);
        assert!(caps.has_message_txid_lookup);
    }

    #[test]
    fn detects_restricted_and_qualifier_assets() {
        let caps = parse_capabilities_from_help(synthetic_help());
        assert!(caps.restricted_assets);
        assert!(caps.qualifiers);
    }

    #[test]
    fn detects_rewards() {
        let caps = parse_capabilities_from_help(synthetic_help());
        assert!(caps.rewards);
    }

    #[test]
    fn detects_snapshots() {
        let caps = parse_capabilities_from_help(synthetic_help());
        assert!(caps.snapshots);
    }

    #[test]
    fn returns_default_for_empty_help() {
        let caps = parse_capabilities_from_help("");
        assert!(!caps.help_probe_success);
        assert!(!caps.wallet_migration);
        assert!(!caps.messaging);
        assert!(!caps.restricted_assets);
        assert!(!caps.qualifiers);
        assert!(!caps.rewards);
        assert!(!caps.snapshots);
        assert!(caps.detected_rpc_names.is_empty());
    }

    #[test]
    fn returns_default_for_no_core_next_rpcs() {
        let help =
            "== Blockchain ==\ngetbestblockhash\ngetblockchaininfo\n== Control ==\nhelp\nstop\n";
        let caps = parse_capabilities_from_help(help);
        assert!(caps.help_probe_success);
        assert!(!caps.wallet_migration);
        assert!(!caps.messaging);
        assert!(!caps.restricted_assets);
        assert!(!caps.qualifiers);
        assert!(!caps.rewards);
        assert!(!caps.snapshots);
        assert!(caps.detected_rpc_names.is_empty());
    }

    #[test]
    fn detects_rpc_names_in_synthetic_help() {
        let caps = parse_capabilities_from_help(synthetic_help());
        assert!(!caps.detected_rpc_names.is_empty());
        let names_lower: Vec<String> = caps
            .detected_rpc_names
            .iter()
            .map(|n| n.to_lowercase())
            .collect();
        let joined = names_lower.join(" ");
        assert!(joined.contains("exportwalletmigration"));
        assert!(joined.contains("getmessaginginfo"));
        assert!(joined.contains("listrestrictedassets"));
        assert!(joined.contains("distributereward"));
        assert!(joined.contains("requestsnapshot"));
    }

    #[test]
    fn compute_sha256_known_content() {
        let dir = std::env::temp_dir().join(format!("hemp0x_sha256_test_{:x}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("test.bin");
        fs::write(&file_path, b"hemp0x identity probe").unwrap();
        let hash = compute_sha256(&file_path).unwrap();
        let _ = fs::remove_dir_all(&dir);
        // SHA256 of "hemp0x identity probe" is deterministic
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn compute_sha256_different_content_different_hash() {
        let dir =
            std::env::temp_dir().join(format!("hemp0x_sha256_test2_{:x}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f1 = dir.join("a.bin");
        let f2 = dir.join("b.bin");
        fs::write(&f1, b"aaaa").unwrap();
        fs::write(&f2, b"bbbb").unwrap();
        let h1 = compute_sha256(&f1).unwrap();
        let h2 = compute_sha256(&f2).unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_ne!(h1, h2);
    }

    #[test]
    fn daemon_process_identity_unavailable_on_empty_port() {
        let identity = get_daemon_process_identity();
        // If no daemon is running on port 42068, result should be unavailable
        if !identity.available {
            assert_eq!(identity.confidence, "none");
            assert!(identity.pid.is_none());
            assert!(identity.exe_path.is_none());
        }
        // If a daemon IS running, it's available and should have a pid
        // This test does not require a daemon to be running.
    }

    #[test]
    fn daemon_process_identity_serializable() {
        let identity = DaemonProcessIdentity::unavailable();
        let json = serde_json::to_string(&identity).unwrap();
        assert!(json.contains("\"available\":false"));
        assert!(json.contains("\"confidence\":\"none\""));
        assert!(json.contains("\"pid\":null"));
    }

    #[test]
    fn parse_commit_hash_matches_required_hash() {
        let raw = "Hemp0x Core Daemon version v4.8.1.0-3e061497a";
        let result = parse_commit_hash(raw);
        assert_eq!(result.as_deref(), Some(REQUIRED_CORE_NEXT_COMMIT));
    }

    #[test]
    fn parse_commit_hash_rejects_different_hash() {
        let raw = "Hemp0x Core Daemon version v4.8.0.0-abcdef1234567890";
        let result = parse_commit_hash(raw);
        assert_ne!(result.as_deref(), Some(REQUIRED_CORE_NEXT_COMMIT));
    }

    #[test]
    fn parse_commit_hash_does_not_match_required_hash_as_substring() {
        let raw = "Hemp0x Core Daemon version v4.8.1.0-03e061497aff";
        let result = parse_commit_hash(raw);
        assert_ne!(result.as_deref(), Some(REQUIRED_CORE_NEXT_COMMIT));
    }

    #[test]
    fn daemon_process_identity_unavailable_is_not_exact() {
        let identity = DaemonProcessIdentity::unavailable();
        assert!(!identity.is_exact_bundled_match());
        assert!(!identity.can_prove_bundled_daemon());
    }

    #[test]
    fn daemon_process_identity_can_prove_requires_sha256_match() {
        let mut identity = DaemonProcessIdentity::unavailable();
        identity.available = true;
        identity.sha256_match = true;
        identity.version_commit_match = true;
        identity.confidence = "high".to_string();
        assert!(!identity.is_exact_bundled_match());
        assert!(identity.can_prove_bundled_daemon());
        identity.sha256_match = false;
        assert!(!identity.can_prove_bundled_daemon());
    }
}
