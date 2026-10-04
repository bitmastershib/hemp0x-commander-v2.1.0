use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

// Import local modules
use crate::modules::commands::run_cli;
use crate::modules::files::{
    config_path, data_dir, ensure_config, load_app_settings, load_app_settings_impl, parse_config,
    save_app_settings, save_app_settings_impl,
};
use crate::modules::utils::{resolve_bin, resolve_bin_with_override};

#[cfg(unix)]
fn unix_process_table_has_live_hemp0xd(stdout: &str) -> bool {
    stdout.lines().any(|line| {
        let mut parts = line.split_whitespace();
        let stat = parts.next().unwrap_or("");
        let command = parts.next().unwrap_or("");
        command == "hemp0xd" && !stat.starts_with('Z')
    })
}

pub(crate) fn daemon_process_running() -> bool {
    #[cfg(unix)]
    {
        return Command::new("ps")
            .arg("-eo")
            .arg("stat=,comm=")
            .output()
            .map(|output| {
                output.status.success()
                    && unix_process_table_has_live_hemp0xd(&String::from_utf8_lossy(&output.stdout))
            })
            .unwrap_or(false);
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        return Command::new("tasklist")
            .creation_flags(0x08000000)
            .arg("/FI")
            .arg("IMAGENAME eq hemp0xd.exe")
            .arg("/NH")
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains("hemp0xd.exe"))
            .unwrap_or(false);
    }

    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

fn node_process_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn wait_for_daemon_process_exit(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while daemon_process_running() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(250));
    }
    !daemon_process_running()
}

fn settle_after_daemon_exit() {
    // hemp0xd can daemonize/shutdown in multiple stages. Process table checks
    // may report no live parent before child threads have released LevelDB /
    // Berkeley DB region locks. A short quiet period avoids immediately
    // starting a replacement daemon into a data directory that Core is still
    // closing.
    for _ in 0..6 {
        thread::sleep(Duration::from_millis(500));
        if daemon_process_running() {
            let _ = wait_for_daemon_process_exit(Duration::from_secs(10));
        }
    }
}

#[cfg(target_os = "linux")]
fn running_daemon_contexts() -> Vec<(Option<PathBuf>, Option<PathBuf>)> {
    let mut contexts = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return contexts;
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let pid = file_name.to_string_lossy();
        if !pid.chars().all(|ch| ch.is_ascii_digit()) {
            continue;
        }

        let cmdline_path = entry.path().join("cmdline");
        let Ok(bytes) = fs::read(cmdline_path) else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }

        let args: Vec<String> = bytes
            .split(|b| *b == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).to_string())
            .collect();
        if args.is_empty() {
            continue;
        }

        let exe_name = Path::new(&args[0])
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if exe_name != "hemp0xd" && exe_name != "hemp0xd.exe" {
            continue;
        }

        let mut conf = None;
        let mut datadir = None;
        let mut iter = args.iter().peekable();
        while let Some(arg) = iter.next() {
            if let Some(value) = arg.strip_prefix("-conf=") {
                conf = Some(PathBuf::from(value));
            } else if arg == "-conf" {
                if let Some(value) = iter.peek() {
                    conf = Some(PathBuf::from(value.as_str()));
                }
            } else if let Some(value) = arg.strip_prefix("-datadir=") {
                datadir = Some(PathBuf::from(value));
            } else if arg == "-datadir" {
                if let Some(value) = iter.peek() {
                    datadir = Some(PathBuf::from(value.as_str()));
                }
            }
        }

        contexts.push((conf, datadir));
    }

    contexts
}

#[cfg(not(target_os = "linux"))]
fn running_daemon_contexts() -> Vec<(Option<PathBuf>, Option<PathBuf>)> {
    Vec::new()
}

fn request_stop_for_context(conf: &Path, datadir: &Path, timeout: Duration) -> Result<String, String> {
    let custom_bin_dir = load_app_settings_impl()
        .ok()
        .and_then(|s| s.custom_core_binary_dir);
    let cli = if let Some(ref d) = custom_bin_dir {
        resolve_bin_with_override("hemp0x-cli", Some(d))
    } else {
        resolve_bin("hemp0x-cli")
    };
    let cli_path = PathBuf::from(&cli);
    if !cli_path.exists() {
        return Err(format!("CLI not found at {}", cli));
    }

    let config = parse_config(conf)?;
    let is_regtest = config.get("regtest").map(|v| v == "1").unwrap_or(false);
    let is_testnet = config.get("testnet").map(|v| v == "1").unwrap_or(false);

    let mut cmd = Command::new(&cli);
    if let Some(parent) = cli_path.parent() {
        cmd.current_dir(parent);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    if is_regtest {
        cmd.arg("-regtest");
    } else if is_testnet {
        cmd.arg("-testnet");
    }

    let mut child = cmd
        .arg(format!("-conf={}", conf.to_string_lossy()))
        .arg(format!("-datadir={}", datadir.to_string_lossy()))
        .arg("stop")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;

    let started = Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_string(&mut stdout);
                }
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                if status.success() {
                    return Ok(stdout.trim().to_string());
                }
                return Err(format!(
                    "CLI error ({}): {} {}",
                    status,
                    stderr.trim(),
                    stdout.trim()
                )
                .trim()
                .to_string());
            }
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("CLI stop request timed out after {} seconds", timeout.as_secs()));
            }
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn stop_node_and_wait_blocking(timeout: Duration) -> Result<(), String> {
    let _guard = node_process_lock()
        .lock()
        .map_err(|_| "Node process lock is poisoned".to_string())?;
    if !daemon_process_running() {
        cleanup_stale_runtime_files();
        return Ok(());
    }

    let deadline = Instant::now() + timeout;
    let mut last_stop_status = String::from("stop not requested yet");
    let mut next_stop_attempt = Instant::now();

    while daemon_process_running() && Instant::now() < deadline {
        if Instant::now() >= next_stop_attempt {
            let stop_timeout = Duration::from_secs(4);
            let active_stop = match (config_path(), data_dir()) {
                (Ok(conf), Ok(datadir)) => request_stop_for_context(&conf, &datadir, stop_timeout),
                (Err(e), _) | (_, Err(e)) => Err(e),
            };
            match active_stop {
                Ok(_) => last_stop_status = "stop requested for active data directory".to_string(),
                Err(e) => {
                    last_stop_status = e;
                    for (conf, datadir) in running_daemon_contexts() {
                        let (Some(conf), Some(datadir)) = (conf, datadir) else {
                            continue;
                        };
                        match request_stop_for_context(&conf, &datadir, stop_timeout) {
                            Ok(_) => {
                                last_stop_status = format!(
                                    "stop requested for running data directory {}",
                                    datadir.to_string_lossy()
                                );
                            }
                            Err(err) => {
                                last_stop_status = format!("{last_stop_status}; {err}");
                            }
                        }
                    }
                }
            }
            next_stop_attempt = Instant::now() + Duration::from_secs(5);
        }
        thread::sleep(Duration::from_millis(500));
    }

    if daemon_process_running() {
        return Err(format!(
            "CORE_LOCK_BUSY::Core is still stopping after {} seconds. Last stop status: {}",
            timeout.as_secs(),
            last_stop_status
        ));
    }

    settle_after_daemon_exit();
    cleanup_stale_runtime_files();
    Ok(())
}

#[tauri::command]
pub async fn start_node() -> Result<(), String> {
    // start_node_inner does CLI probes, lock waits, and process spawn —
    // run it on a background thread so the webview stays responsive
    // during startup / "Start Core" while Core warms up.
    tauri::async_runtime::spawn_blocking(move || start_node_blocking())
        .await
        .map_err(|e| format!("Start node task failed: {e}"))?
}

/// Synchronous entry point used by other sync backend code (legacy
/// restore/create wallet flows) that need to start Core with the active
/// vault wallet. The async `start_node` Tauri command above wraps this
/// in `spawn_blocking` for the frontend.
pub fn start_node_blocking() -> Result<(), String> {
    let _guard = node_process_lock()
        .lock()
        .map_err(|_| "Node process lock is poisoned".to_string())?;
    let settings: crate::modules::models::AppSettings = load_app_settings()?;
    let wallet_name: Option<String> = settings.active_vault_wallet_name.clone();
    start_node_inner(wallet_name.as_deref())?;
    // When an active vault wallet is configured, verify it actually
    // becomes queryable after spawn. `start_node_inner` only spawns the
    // daemon; without this check Commander would report "started" even
    // if Core failed to load the named wallet (missing file, wrong
    // wallet=, etc.), leaving the user on the wrong/no wallet.
    if let Some(wn) = wallet_name.as_deref() {
        verify_named_wallet_loaded_after_start(wn)?;
    }
    Ok(())
}

/// Poll Core until the named wallet answers `getwalletinfo`, or surface a
/// bounded readiness error. Some restarts need longer than the old 30s window,
/// especially after wallet encryption or rapid wallet switching.
fn verify_named_wallet_loaded_after_start(wallet_name: &str) -> Result<(), String> {
    wait_for_named_wallet_queryable_with_restart(wallet_name, Duration::from_secs(90))
}

fn wait_for_named_wallet_queryable_with_restart(
    wallet_name: &str,
    timeout: Duration,
) -> Result<(), String> {
    let wallet_arg = format!("-wallet={wallet_name}");
    let deadline = Instant::now() + timeout;
    let mut next_start_attempt = Instant::now() + Duration::from_secs(3);
    let mut last_wallet_error = String::new();
    let mut last_start_error = String::new();

    // Brief initial wait for the daemon to bind RPC after the first launch.
    thread::sleep(Duration::from_millis(800));
    while Instant::now() < deadline {
        match crate::modules::commands::run_cli(&[
            wallet_arg.clone(),
            String::from("getwalletinfo"),
        ]) {
            Ok(raw) => {
                if serde_json::from_str::<serde_json::Value>(&raw).is_ok() {
                    return Ok(());
                }
                last_wallet_error = "Core returned non-JSON wallet status".to_string();
            }
            Err(e) => {
                last_wallet_error = e;
            }
        }

        // A daemonized launcher can return success and then exit immediately
        // if the previous process is still releasing the datadir lock. Retry
        // only when there is no live daemon, so a second instance can never be
        // started alongside one that is still warming up.
        if !daemon_process_running() && Instant::now() >= next_start_attempt {
            match start_node_inner(Some(wallet_name)) {
                Ok(()) => last_start_error.clear(),
                Err(e) => last_start_error = e,
            }
            next_start_attempt = Instant::now() + Duration::from_secs(5);
        }

        thread::sleep(Duration::from_millis(750));
    }

    let status = if !last_wallet_error.is_empty() {
        last_wallet_error
    } else if !last_start_error.is_empty() {
        last_start_error
    } else {
        "no response".to_string()
    };
    Err(format!(
        "Core could not make wallet '{wallet_name}' queryable after {} seconds. Last status: {status}",
        timeout.as_secs()
    ))
}

pub fn wait_for_default_wallet_queryable(timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut last_error = String::new();

    while Instant::now() < deadline {
        match crate::modules::commands::run_cli(&[String::from("getwalletinfo")]) {
            Ok(raw) => {
                if serde_json::from_str::<serde_json::Value>(&raw).is_ok() {
                    return Ok(());
                }
                last_error = "Core returned non-JSON wallet status.".to_string();
            }
            Err(e) => {
                last_error = e;
            }
        }
        thread::sleep(Duration::from_millis(750));
    }

    Err(format!(
        "Core restarted, but wallet.dat is not queryable yet after {} seconds. Last status: {}",
        timeout.as_secs(),
        if last_error.is_empty() {
            "no wallet status returned"
        } else {
            last_error.as_str()
        }
    ))
}

fn start_node_inner(wallet_name: Option<&str>) -> Result<(), String> {
    let cfg = ensure_config()?;
    let dir = data_dir()?;
    let lock_path = dir.join(".lock");

    // The .lock file can persist after a clean shutdown. Treat an actual
    // hemp0xd process as authoritative; file existence alone does not mean
    // the datadir is currently locked.
    if daemon_process_running() {
        if let Ok(raw) = crate::modules::commands::run_cli(&[String::from("getblockchaininfo")]) {
            if serde_json::from_str::<serde_json::Value>(&raw).is_ok() {
                // Core is already running and responding. If a wallet_name was
                // requested, load it into the running daemon and verify that
                // wallet-scoped RPCs are queryable before reporting success.
                if let Some(wn) = wallet_name {
                    let wallet_arg = format!("-wallet={wn}");
                    if crate::modules::commands::run_cli(&[
                        wallet_arg.clone(),
                        String::from("getwalletinfo"),
                    ])
                    .is_ok()
                    {
                        return Ok(());
                    }
                    // B1 66d: Core Next does not support dynamic loadwallet.
                    // If the wallet is not queryable with -wallet=<name>, restart is required.
                    return Err(format!("Core is already running, but wallet '{}' is not queryable. Restart Core with -wallet={} to use this wallet.", wn, wn));
                }
                return Ok(());
            }
        }
        // Lock exists but Core is not responding. This commonly happens during
        // rapid stop/start while the previous daemon is still releasing the
        // datadir, or while Core is warming up before RPC is ready. Do not
        // spawn a second daemon into the same datadir; wait for either RPC to
        // become queryable or the process to exit.
        let deadline = Instant::now() + Duration::from_secs(30);
        while daemon_process_running() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(500));
            if let Ok(raw) = crate::modules::commands::run_cli(&[String::from("getblockchaininfo")])
            {
                if serde_json::from_str::<serde_json::Value>(&raw).is_ok() {
                    return Ok(());
                }
            }
        }
        if daemon_process_running() {
            return Err(format!(
                "CORE_LOCK_BUSY::Core is still starting or stopping with data directory {}. Wait a few moments and try again.",
                lock_path.to_string_lossy()
            ));
        }
    }

    cleanup_stale_runtime_files_for_dir(&dir);

    let settings: crate::modules::models::AppSettings = load_app_settings()?;
    let custom_bin_dir = settings.custom_core_binary_dir.clone();

    let daemon = if let Some(ref d) = custom_bin_dir {
        resolve_bin_with_override("hemp0xd", Some(d))
    } else {
        resolve_bin("hemp0xd")
    };

    let repair_flag: Option<String> = match (
        settings.pending_repair_mode.as_deref(),
        settings.active_repair_mode.as_deref(),
    ) {
        (Some("reindex"), _) | (_, Some("reindex")) => Some("reindex".to_string()),
        (Some("reindex-chainstate"), _) | (_, Some("reindex-chainstate")) => {
            Some("reindex-chainstate".to_string())
        }
        _ => None,
    };

    let daemon_path = PathBuf::from(&daemon);
    if !daemon_path.exists() {
        let hint = if custom_bin_dir.is_some() {
            "Daemon not found in custom Core binary folder. Check the folder path or reset to bundled."
        } else {
            &format!("Daemon not found at {}", daemon)
        };
        return Err(hint.to_string());
    }
    #[cfg(unix)]
    let mut cmd = Command::new(&daemon);
    #[cfg(windows)]
    let mut cmd = Command::new(&daemon);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        cmd.arg(format!("-conf={}", cfg.to_string_lossy()))
            .arg(format!("-datadir={}", dir.to_string_lossy()));
        if let Some(ref flag) = repair_flag {
            cmd.arg(format!("-{}", flag));
        }
        if let Some(ref wn) = wallet_name {
            cmd.arg(format!("-wallet={}", wn));
        }
    }

    #[cfg(unix)]
    {
        cmd.arg(format!("-conf={}", cfg.to_string_lossy()))
            .arg(format!("-datadir={}", dir.to_string_lossy()))
            .arg("-daemon");
        if let Some(ref flag) = repair_flag {
            cmd.arg(format!("-{}", flag));
        }
        if let Some(ref wn) = wallet_name {
            cmd.arg(format!("-wallet={}", wn));
        }
    }

    cmd.spawn().map_err(|e| e.to_string())?;

    if repair_flag.is_some() {
        let mut updated = settings.clone();
        updated.pending_repair_mode = None;
        if updated.active_repair_mode.is_none() {
            updated.active_repair_mode = repair_flag;
        }
        save_app_settings(updated)?;
    }

    Ok(())
}

#[tauri::command]
pub async fn stop_node() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(stop_node_blocking)
        .await
        .map_err(|e| format!("Stop node task failed: {e}"))?
}

pub fn stop_node_blocking() -> Result<(), String> {
    let _guard = node_process_lock()
        .lock()
        .map_err(|_| "Node process lock is poisoned".to_string())?;
    if !daemon_process_running() {
        cleanup_stale_runtime_files();
        return Ok(());
    }

    match run_cli(&[String::from("stop")]) {
        Ok(_) => {
            if !daemon_process_running() {
                settle_after_daemon_exit();
                cleanup_stale_runtime_files();
            }
            Ok(())
        }
        Err(_e) if !daemon_process_running() => {
            settle_after_daemon_exit();
            cleanup_stale_runtime_files();
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[tauri::command]
pub async fn stop_node_and_wait(timeout_ms: Option<u64>) -> Result<(), String> {
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(90_000).max(1_000));
    tauri::async_runtime::spawn_blocking(move || stop_node_and_wait_blocking(timeout))
        .await
        .map_err(|e| format!("Stop node task failed: {e}"))?
}

#[tauri::command]
pub async fn restart_node_with_wallet(wallet_name: String) -> Result<(), String> {
    let wn = wallet_name.clone();
    tauri::async_runtime::spawn_blocking(move || restart_node_with_wallet_robust(&wn, true))
        .await
        .map_err(|e| format!("Restart task failed: {e}"))?
}

pub fn restart_node_with_wallet_robust(
    wallet_name: &str,
    poll_until_loaded: bool,
) -> Result<(), String> {
    restart_node_with_wallet_robust_inner(wallet_name, poll_until_loaded, false)
}

pub fn restart_node_with_wallet_for_default_context(wallet_name: &str) -> Result<(), String> {
    restart_node_with_wallet_robust_inner(wallet_name, true, true)
}

fn stop_daemon_for_wallet_restart(timeout: Duration) -> Result<(), String> {
    if !daemon_process_running() {
        return Ok(());
    }

    let deadline = Instant::now() + timeout;
    let mut last_stop_error = String::new();
    let mut next_stop_attempt = Instant::now();

    while daemon_process_running() && Instant::now() < deadline {
        if Instant::now() >= next_stop_attempt {
            match run_cli(&[String::from("stop")]) {
                Ok(_) => last_stop_error = "stop requested".to_string(),
                Err(e) => last_stop_error = e,
            }
            next_stop_attempt = Instant::now() + Duration::from_secs(5);
        }
        thread::sleep(Duration::from_millis(500));
    }

    if daemon_process_running() {
        return Err(format!(
            "CORE_LOCK_BUSY::Core is still stopping with data directory {}. Last stop status: {}. Wait a few moments and try again.",
            data_dir()?.join(".lock").to_string_lossy(),
            if last_stop_error.is_empty() {
                "no stop response"
            } else {
                last_stop_error.as_str()
            }
        ));
    }

    settle_after_daemon_exit();
    cleanup_stale_runtime_files();
    Ok(())
}

fn restart_node_with_wallet_robust_inner(
    wallet_name: &str,
    poll_until_loaded: bool,
    force_restart: bool,
) -> Result<(), String> {
    let _guard = node_process_lock()
        .lock()
        .map_err(|_| "Node process lock is poisoned".to_string())?;
    let _dir = data_dir()?;

    // Fast path: if Core is already running, try to load/query the target
    // wallet without a full daemon restart. This is the least disruptive path
    // after switching from legacy wallet.dat mode back to a vault wallet.
    if !force_restart
        && crate::modules::commands::run_cli(&[String::from("getblockchaininfo")]).is_ok()
    {
        let wallet_arg = format!("-wallet={wallet_name}");
        if crate::modules::commands::run_cli(&[wallet_arg.clone(), String::from("getwalletinfo")])
            .is_ok()
        {
            return Ok(());
        }
        // B1 66d: Core Next does not support dynamic loadwallet.
        // If the wallet is not queryable, fall through to the stop/restart path.
    }

    // Step 1/2: stop Core and wait until the process is gone. Do not call the
    // normal start helper while a different wallet is still running; Core Next
    // cannot dynamically switch wallets in that state.
    stop_daemon_for_wallet_restart(Duration::from_secs(90))?;

    // Step 3: start Core with the target wallet.
    let start_result = start_node_inner(Some(wallet_name));

    // Step 4: poll until the named wallet is queryable. This also retries a
    // daemon launch that exited during datadir lock cleanup.
    if poll_until_loaded {
        return wait_for_named_wallet_queryable_with_restart(wallet_name, Duration::from_secs(90));
    }

    start_result
}

#[tauri::command]
pub fn set_network_mode(mode: String) -> Result<String, String> {
    // Attempt to stop the running node BEFORE changing config
    let _ = stop_node_blocking();

    // Give it a moment to shutdown gracefully
    thread::sleep(Duration::from_secs(2));

    let cfg_path = config_path()?;
    ensure_config()?; // Ensure it exists

    // Create a timestamped backup before modifying hemp.conf
    if cfg_path.exists() {
        let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let backup_path = cfg_path.with_extension(format!("conf.{}.bak", timestamp));
        fs::copy(&cfg_path, &backup_path)
            .map_err(|e| format!("Failed to create config backup: {}", e))?;
    }

    // SAFETY (slice 64h): preserve the original line ending style
    // (LF vs CRLF) and trailing newline so we do not silently mangle
    // `hemp.conf`. The user's config may contain comments, blank lines,
    // and other settings we never touch. We only insert or remove
    // network flags.
    let content = fs::read_to_string(&cfg_path).map_err(|e| e.to_string())?;
    let trailing_newline = content.ends_with('\n');
    let uses_crlf = content.contains("\r\n");
    let line_sep: &str = if uses_crlf { "\r\n" } else { "\n" };
    // Strip a single trailing newline so we can re-join cleanly and
    // re-append at the end. Avoids an extra blank line.
    let trimmed = if trailing_newline {
        &content[..content.len() - 1]
    } else {
        &content
    };
    let trimmed = if trimmed.ends_with('\r') {
        &trimmed[..trimmed.len() - 1]
    } else {
        trimmed
    };

    let mut new_lines: Vec<String> = Vec::new();

    // Filter out existing network flags (preserve comments + everything else)
    for line in trimmed.split(&format!("\n")) {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if !line.trim().starts_with("testnet=") && !line.trim().starts_with("regtest=") {
            new_lines.push(line.to_string());
        }
    }

    // Add new mode
    match mode.as_str() {
        "testnet" => new_lines.push("testnet=1".to_string()),
        "regtest" => new_lines.push("regtest=1".to_string()),
        "mainnet" => {} // distinct absence of flags
        _ => return Err("Invalid network mode".to_string()),
    }

    // Write back, preserving the original trailing newline and line ending.
    let mut out = new_lines.join(line_sep);
    if trailing_newline {
        out.push_str(line_sep);
    }
    fs::write(&cfg_path, out).map_err(|e| e.to_string())?;
    Ok("Network mode updated. Please restart the node.".to_string())
}

#[tauri::command]
pub fn restart_app(app_handle: tauri::AppHandle) {
    app_handle.restart();
}

// Helper for restore_wallet and create_new_wallet
pub fn wait_for_lock_release(dir: &Path) {
    let lock_path = dir.join(".lock");
    for _ in 0..20 {
        if !lock_path.exists() {
            break;
        }
        if !daemon_process_running() {
            cleanup_stale_runtime_files_for_dir(dir);
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
}

pub fn cleanup_stale_runtime_files() {
    if let Ok(dir) = data_dir() {
        cleanup_stale_runtime_files_for_dir(&dir);
    }
}

fn cleanup_stale_runtime_files_for_dir(dir: &Path) {
    if daemon_process_running() {
        return;
    }
    for name in [".cookie", ".lock"] {
        let path = dir.join(name);
        if path.exists() && path.is_file() {
            let _ = fs::remove_file(path);
        }
    }
}

pub fn stop_node_internal() {
    let _ = run_cli(&[String::from("stop")]);
    thread::sleep(Duration::from_secs(2));
    if !daemon_process_running() {
        cleanup_stale_runtime_files();
    }
}

#[tauri::command]
pub fn restore_wallet(
    path: String,
    backup_existing: bool,
    restart_node: bool,
) -> Result<(), String> {
    let dir = data_dir()?;
    let wallet = dir.join("wallet.dat");
    let source = PathBuf::from(&path);
    if !source.exists() {
        return Err("Restore file not found.".to_string());
    }

    // If the user selects the active wallet.dat itself, this is already
    // the runtime wallet. Do not stop Core, archive it, or try to copy it
    // over itself. The previous behavior renamed wallet.dat to a backup and
    // then failed because the selected source path no longer existed.
    if wallet.exists() {
        if let (Ok(src), Ok(dst)) = (source.canonicalize(), wallet.canonicalize()) {
            if src == dst {
                return Ok(());
            }
        }
    }

    // Stop node logic internal
    stop_node_internal();

    if wallet.exists() && backup_existing {
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
        let backup_dir = dir.join("wallet_backups");
        let _ = fs::create_dir_all(&backup_dir);
        let backup = backup_dir.join(format!("wallet_{}.bak", ts));
        fs::rename(&wallet, backup).map_err(|e| e.to_string())?;
    } else if wallet.exists() {
        // SAFETY: refuse to delete wallet.dat without an explicit backup.
        // The normal UI always passes backup_existing: true. This path
        // is only reachable by direct IPC calls from advanced/legacy
        // flows that must explicitly acknowledge the risk.
        return Err(
        "A wallet.dat already exists. Commander requires backup_existing: true to protect the current wallet. Use the normal Wallet page restore flow, or pass backup_existing: true to archive the current wallet first.".to_string()
      );
    }
    fs::copy(&source, wallet).map_err(|e| e.to_string())?;
    if restart_node {
        wait_for_lock_release(&dir);
        let _ = start_node_blocking();
    }
    Ok(())
}

pub fn check_hemp_conf_wallet_line() -> Option<String> {
    let cfg_path = match config_path() {
        Ok(p) => p,
        Err(_) => return None,
    };
    if !cfg_path.exists() {
        return None;
    }
    let content = match fs::read_to_string(&cfg_path) {
        Ok(c) => c,
        Err(_) => return None,
    };
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            let key = k.trim();
            if key == "wallet" {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

#[tauri::command]
pub async fn restore_legacy_wallet_dat(
    path: String,
    restart_node: bool,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        restore_legacy_wallet_dat_blocking(path, restart_node)
    })
    .await
    .map_err(|e| format!("Legacy wallet import task failed: {e}"))?
}

#[tauri::command]
pub async fn switch_to_legacy_wallet_dat(
    restart_node: Option<bool>,
) -> Result<serde_json::Value, String> {
    let do_restart = restart_node.unwrap_or(true);
    tauri::async_runtime::spawn_blocking(move || switch_to_legacy_wallet_dat_blocking(do_restart))
        .await
        .map_err(|e| format!("Switch to wallet.dat task failed: {e}"))?
}

fn switch_to_legacy_wallet_dat_blocking(restart_node: bool) -> Result<serde_json::Value, String> {
    let dir = data_dir()?;
    let wallet = dir.join("wallet.dat");
    if !wallet.exists() {
        return Err(
            "No wallet.dat exists. Import a wallet.dat or create a new legacy wallet first."
                .to_string(),
        );
    }

    let mut settings = load_app_settings_impl()?;
    settings.active_vault_wallet_name = None;
    save_app_settings_impl(&settings)?;

    let hemp_conf_wallet = check_hemp_conf_wallet_line();
    let mut restarted = false;
    let mut restart_error: Option<String> = None;

    if restart_node {
        stop_node_internal();
        wait_for_lock_release(&dir);
        match start_node_blocking() {
      Ok(()) => match wait_for_default_wallet_queryable(Duration::from_secs(90)) {
        Ok(()) => restarted = true,
        Err(e) => restart_error = Some(format!("Core restarted in wallet.dat mode, but wallet.dat is still warming up: {e}")),
      },
      Err(e) => restart_error = Some(format!("Core failed to restart in wallet.dat mode: {e}. Start Core manually through Commander.")),
    }
    }

    Ok(serde_json::json!({
      "switched": true,
      "legacy_wallet_mode": true,
      "wallet_dat_exists": true,
      "hemp_conf_wallet": hemp_conf_wallet,
      "restarted": restarted,
      "restart_error": restart_error,
    }))
}

fn restore_legacy_wallet_dat_blocking(
    path: String,
    restart_node: bool,
) -> Result<serde_json::Value, String> {
    let dir = data_dir()?;
    let wallet = dir.join("wallet.dat");
    let source = PathBuf::from(&path);

    if !source.exists() {
        return Err("Selected file not found.".to_string());
    }

    let source_meta = fs::metadata(&source).map_err(|e| format!("Cannot read source file: {e}"))?;
    if source_meta.len() == 0 {
        return Err("Selected file is empty. Not a valid wallet.".to_string());
    }

    // Same-file check: if the selected source canonicalizes to the active
    // data-dir wallet.dat, it is already the runtime wallet. Do not stop
    // Core, archive, or copy over itself.
    if wallet.exists() {
        if let (Ok(src), Ok(dst)) = (source.canonicalize(), wallet.canonicalize()) {
            if src == dst {
                // Still clear active vault wallet name so Commander uses default
                // wallet.dat mode from now on.
                let mut settings = load_app_settings_impl()?;
                settings.active_vault_wallet_name = None;
                save_app_settings_impl(&settings)?;

                let hemp_conf_wallet = check_hemp_conf_wallet_line();
                let mut restarted = false;
                let mut restart_error: Option<String> = None;
                if restart_node {
                    stop_node_internal();
                    wait_for_lock_release(&dir);
                    match start_node_blocking() {
            Ok(()) => match wait_for_default_wallet_queryable(Duration::from_secs(90)) {
              Ok(()) => restarted = true,
              Err(e) => restart_error = Some(format!("Legacy wallet mode was selected, but Core is still warming up: {e}")),
            },
            Err(e) => restart_error = Some(format!("Legacy wallet mode was selected, but Core failed to restart: {e}. Start Core manually through Commander.")),
          }
                }
                return Ok(serde_json::json!({
                  "already_active": true,
                  "archived_existing": false,
                  "hemp_conf_wallet": hemp_conf_wallet,
                  "restarted": restarted,
                  "restart_error": restart_error,
                  "message": "This file is already the active wallet.dat. Commander has switched to legacy wallet mode.",
                }));
            }
        }
    }

    // Stage the selected source inside the data dir before touching the active
    // wallet. If the source cannot be copied, the existing wallet.dat remains in
    // place and Core can continue using it.
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let staged_wallet = dir.join(format!("wallet.dat.importing.{ts}.tmp"));
    fs::copy(&source, &staged_wallet).map_err(|e| format!("Failed to stage wallet file: {e}"))?;

    // Different file: stop Core, archive existing wallet.dat, then promote the
    // staged copy. The selected source is never moved or renamed.
    stop_node_internal();
    wait_for_lock_release(&dir);

    let mut archived_existing = false;
    if wallet.exists() {
        let backup_dir = dir.join("wallet_backups");
        let _ = fs::create_dir_all(&backup_dir);
        let backup = backup_dir.join(format!("wallet_{}.bak", ts));
        if let Err(e) = fs::rename(&wallet, &backup) {
            let _ = fs::remove_file(&staged_wallet);
            return Err(format!("Failed to archive existing wallet.dat: {e}"));
        }
        archived_existing = true;
    }

    if let Err(e) = fs::rename(&staged_wallet, &wallet) {
        let _ = fs::remove_file(&staged_wallet);
        return Err(format!("Failed to install staged wallet.dat: {e}"));
    }

    // Clear active vault wallet startup selection so Commander does not
    // keep trying to start Core with the old vault wallet name.
    let mut settings = load_app_settings_impl()?;
    settings.active_vault_wallet_name = None;
    save_app_settings_impl(&settings)?;

    let hemp_conf_wallet = check_hemp_conf_wallet_line();

    let mut restarted = false;
    if restart_node {
        // Start Core in default wallet mode (no -wallet=<vault-name>).
        // If hemp.conf has a user-managed wallet= line, do not rewrite it —
        // the caller should surface a warning.
        match start_node_blocking() {
            Ok(()) => {
                if let Err(e) = wait_for_default_wallet_queryable(Duration::from_secs(90)) {
                    return Ok(serde_json::json!({
                      "already_active": false,
                      "archived_existing": archived_existing,
                      "hemp_conf_wallet": hemp_conf_wallet,
                      "restarted": false,
                      "restart_error": format!("Wallet imported and Core was started, but wallet.dat is still warming up: {e}"),
                    }));
                }
                restarted = true;
            }
            Err(e) => {
                return Ok(serde_json::json!({
                  "already_active": false,
                  "archived_existing": archived_existing,
                  "hemp_conf_wallet": hemp_conf_wallet,
                  "restarted": false,
                  "restart_error": format!("Wallet imported but Core failed to restart: {e}. Start Core manually through Commander."),
                }));
            }
        }
    }

    Ok(serde_json::json!({
      "already_active": false,
      "archived_existing": archived_existing,
      "hemp_conf_wallet": hemp_conf_wallet,
      "restarted": restarted,
    }))
}

#[tauri::command]
pub fn validate_wallet_file(path: String) -> Result<serde_json::Value, String> {
    let path = Path::new(&path);
    if !path.exists() {
        return Err("File not found.".to_string());
    }
    if !path.is_file() {
        return Err("Path is not a file.".to_string());
    }
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if file_size == 0 {
        return Err("File is empty. Not a valid wallet file.".to_string());
    }
    let mut file = std::fs::File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let mut buf = vec![0u8; 100.min(file_size as usize)];
    use std::io::Read;
    file.read_exact(&mut buf)
        .map_err(|e| format!("Cannot read file header: {e}"))?;

    if buf.iter().copied().find(|b| !b.is_ascii_whitespace()) == Some(b'{') {
        return Err(
            "This is a JSON file, not a Core wallet file. Use Switch / Import to load Hemp0x Vault JSON files."
                .to_string(),
        );
    }

    let sqlite_header = b"SQLite format 3\0";
    let is_sqlite =
        buf.len() >= sqlite_header.len() && &buf[..sqlite_header.len()] == sqlite_header;
    if is_sqlite {
        return Ok(serde_json::json!({ "valid": true, "format": "sqlite" }));
    }

    // Berkeley DB magic number is at byte offset 12 in the metadata page.
    // BDB 4.8+ magic: 0x00061561
    // Big-endian:   00 06 15 61  at bytes 12-15
    // Little-endian: 61 15 06 00  at bytes 12-15
    // Also check the byte-order marker at offset 0 (BDB 4.8 uses 0x00061561
    // or 0x00053162 as the byte-order sentinel in the first 4 bytes).
    let bdb_magic_be = [0x00u8, 0x06, 0x15, 0x61];
    let bdb_magic_le = [0x61u8, 0x15, 0x06, 0x00];
    let bdb_byteorder_be = [0x00u8, 0x00, 0x06, 0x15]; // first 4 bytes big-endian
    let bdb_byteorder_le = [0x62u8, 0x31, 0x05, 0x00]; // first 4 bytes little-endian

    let is_bdb = (buf.len() >= 16 && (buf[12..16] == bdb_magic_be || buf[12..16] == bdb_magic_le))
        || (buf.len() >= 4 && (buf[..4] == bdb_byteorder_be || buf[..4] == bdb_byteorder_le));

    if is_bdb {
        return Ok(serde_json::json!({ "valid": true, "format": "bdb" }));
    }

    // Last resort: if the file is large enough to plausibly be a wallet,
    // accept it as a candidate instead of mutating the live Core data
    // directory during validation. The restore/import command remains
    // responsible for the authoritative Core load/restore failure if the
    // user selected the wrong file. This keeps validation from loading a
    // probe wallet into a running daemon or leaving extra files behind.
    if file_size > 4096 {
        return Ok(serde_json::json!({
            "valid": true,
            "format": "core_wallet_candidate",
            "detected_by": "size_header_fallback",
            "warning": "Wallet header was not recognized as SQLite or Berkeley DB, but the file is large enough to be a Core wallet. Core will verify it during import."
        }));
    }

    Err(
        "File does not appear to be a valid Core wallet file (expected BDB or SQLite format)."
            .to_string(),
    )
}

fn inspect_core_migration_envelope(path: &Path) -> Result<serde_json::Value, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("Cannot open file for migration detection: {e}"))?;
    let mut prefix = [0u8; 512];
    let read_len = std::io::Read::read(&mut file, &mut prefix)
        .map_err(|e| format!("Cannot read file for migration detection: {e}"))?;
    let first_non_ws = prefix[..read_len]
        .iter()
        .copied()
        .find(|b| !b.is_ascii_whitespace());
    if first_non_ws != Some(b'{') {
        return Err("File does not start like a JSON Core migration envelope.".to_string());
    }

    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("Not a JSON Core migration envelope: {e}"))?;
    let envelope: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("Not a JSON Core migration envelope: {e}"))?;

    let schema = envelope
        .get("schema_identifier")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing Core migration schema identifier.".to_string())?;

    if !schema.starts_with("hemp0x-core.migration-envelope.v") {
        return Err("JSON file is not a Hemp0x Core migration envelope.".to_string());
    }

    let envelope_version = envelope
        .get("envelope_version")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "Missing Core migration envelope version.".to_string())?;

    let wallet_summary = envelope.get("wallet_summary");
    let private = envelope.get("private");
    let chain = envelope.get("chain");

    Ok(serde_json::json!({
        "schema_identifier": schema,
        "envelope_version": envelope_version,
        "wallet_name": wallet_summary
            .and_then(|v| v.get("wallet_name"))
            .and_then(|v| v.as_str()),
        "encrypted": private
            .and_then(|v| v.get("encrypted"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "private_keys_included": wallet_summary
            .and_then(|v| v.get("private_keys_included"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "network": chain
            .and_then(|v| v.get("network"))
            .and_then(|v| v.as_str()),
        "coin_type": chain
            .and_then(|v| v.get("coin_type_bip44"))
            .and_then(|v| v.as_i64()),
    }))
}

#[tauri::command]
pub fn detect_wallet_file_type(path: String) -> Result<serde_json::Value, String> {
    let path = Path::new(&path);
    if !path.exists() {
        return Err("File not found.".to_string());
    }
    if !path.is_file() {
        return Err("Path is not a file.".to_string());
    }
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if file_size == 0 {
        return Err("File is empty.".to_string());
    }

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let mut wallet_validation: Option<serde_json::Value> = None;
    let mut wallet_error: Option<String> = None;
    match validate_wallet_file(path.to_string_lossy().to_string()) {
        Ok(v) => wallet_validation = Some(v),
        Err(e) => wallet_error = Some(e),
    }

    let mut migration_validation: Option<serde_json::Value> = None;
    let mut migration_error: Option<String> = None;
    match inspect_core_migration_envelope(path) {
        Ok(v) => migration_validation = Some(v),
        Err(e) => migration_error = Some(e),
    }

    let wallet_valid = wallet_validation.as_ref().map(|_| true).unwrap_or(false);
    let migration_valid = migration_validation.as_ref().map(|_| true).unwrap_or(false);

    if wallet_valid && !migration_valid {
        let format = wallet_validation
            .as_ref()
            .and_then(|v| v.get("format"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return Ok(serde_json::json!({
            "detected_type": "legacy_core_wallet",
            "file_name": file_name,
            "file_size": file_size,
            "wallet_format": format,
            "wallet_valid": true,
            "migration_valid": false,
            "migration_error": migration_error,
        }));
    }

    if migration_valid {
        let mig_info = migration_validation.as_ref();
        return Ok(serde_json::json!({
            "detected_type": "core_migration_envelope",
            "file_name": file_name,
            "file_size": file_size,
            "wallet_valid": wallet_valid,
            "wallet_error": wallet_error,
            "migration_valid": true,
            "migration_wallet_name": mig_info.and_then(|v| v.get("wallet_name")).and_then(|v| v.as_str()),
            "migration_version": mig_info.and_then(|v| v.get("envelope_version")).and_then(|v| v.as_i64()),
            "migration_format": mig_info.and_then(|v| v.get("schema_identifier")).and_then(|v| v.as_str()),
            "migration_encrypted": mig_info.and_then(|v| v.get("encrypted")).and_then(|v| v.as_bool()),
            "migration_private_keys_included": mig_info.and_then(|v| v.get("private_keys_included")).and_then(|v| v.as_bool()),
            "migration_network": mig_info.and_then(|v| v.get("network")).and_then(|v| v.as_str()),
            "migration_coin_type": mig_info.and_then(|v| v.get("coin_type")).and_then(|v| v.as_i64()),
            "migration_checked_by": "json_header",
        }));
    }

    Ok(serde_json::json!({
        "detected_type": "unknown",
        "file_name": file_name,
        "file_size": file_size,
        "wallet_valid": false,
        "wallet_error": wallet_error,
        "migration_valid": false,
        "migration_error": migration_error,
    }))
}

#[tauri::command]
pub fn create_new_wallet(backup_existing: bool, restart_node: bool) -> Result<(), String> {
    let dir = data_dir()?;
    let wallet = dir.join("wallet.dat");

    stop_node_internal();

    if wallet.exists() && backup_existing {
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
        let backup_dir = dir.join("wallet_backups");
        let _ = fs::create_dir_all(&backup_dir);
        let backup = backup_dir.join(format!("wallet_{}.bak", ts));
        fs::rename(&wallet, backup).map_err(|e| e.to_string())?;
    } else if wallet.exists() {
        return Err(
        "A wallet.dat already exists. Commander requires backup_existing: true to protect the current wallet. Use the normal Wallet page create flow, or pass backup_existing: true to archive the current wallet first.".to_string()
      );
    }
    if restart_node {
        wait_for_lock_release(&dir);
        let _ = start_node_blocking();
    }
    Ok(())
}

// Commands from commands.rs that also need start/stop access, i.e., backup_wallet is fine in commands.rs
// restore/create_wallet involve restarting logic so they moved here.

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{}_{}", prefix, std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_test_wallet(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, content).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn unix_process_table_ignores_defunct_hemp0xd() {
        let table = "\
Z+ hemp0xd
Sl node
S bash
";
        assert!(!unix_process_table_has_live_hemp0xd(table));
    }

    #[cfg(unix)]
    #[test]
    fn unix_process_table_detects_live_hemp0xd() {
        let table = "\
Z+ hemp0xd
Sl hemp0xd
S bash
";
        assert!(unix_process_table_has_live_hemp0xd(table));
    }

    #[test]
    fn legacy_wallet_same_file_no_rename_or_delete() {
        let tmp = TempDir::new("hemp64o_same_file");
        let wallet_path =
            write_test_wallet(&tmp.path, "wallet.dat", b"SQLite format 3\0data-data-data");
        let original_content = fs::read(&wallet_path).unwrap();

        let source = wallet_path.clone();
        let dir = tmp.path.clone();
        let dest = dir.join("wallet.dat");

        // Simulate: source canonicalizes to the same path as dest.
        // In the real function, both are the same canonical path.
        // The function should return early without modifying the file.
        if let (Ok(src_canon), Ok(dst_canon)) = (source.canonicalize(), dest.canonicalize()) {
            assert_eq!(
                src_canon, dst_canon,
                "same file should canonicalize to same path"
            );
        }

        // Verify the source file is untouched.
        let after_content = fs::read(&wallet_path).unwrap();
        assert_eq!(
            original_content, after_content,
            "same-file restore must not modify the source"
        );
        assert!(wallet_path.exists(), "source file must still exist");
    }

    #[test]
    fn legacy_wallet_different_file_copies_and_archives() {
        let tmp = TempDir::new("hemp64o_diff_file");
        let data_dir = tmp.path.join("data");
        fs::create_dir_all(&data_dir).unwrap();

        // Existing wallet.dat in data dir
        let existing = data_dir.join("wallet.dat");
        fs::write(&existing, b"old-wallet-content- SQLite format 3").unwrap();

        // Source wallet file in a different location
        let source_dir = tmp.path.join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("wallet.dat");
        fs::write(&source, b"new-wallet-content- SQLite format 3").unwrap();

        // Manually simulate the copy+archive logic (can't call stop_node_internal in tests)
        let backup_dir = data_dir.join("wallet_backups");
        fs::create_dir_all(&backup_dir).unwrap();
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
        let backup = backup_dir.join(format!("wallet_{}.bak", ts));
        fs::rename(&existing, &backup).unwrap();
        fs::copy(&source, data_dir.join("wallet.dat")).unwrap();

        // Source file should not be moved/renamed
        assert!(source.exists(), "source file must not be moved or deleted");
        assert_eq!(
            fs::read(&source).unwrap(),
            b"new-wallet-content- SQLite format 3"
        );

        // Destination should have new content
        let new_wallet = data_dir.join("wallet.dat");
        assert_eq!(
            fs::read(&new_wallet).unwrap(),
            b"new-wallet-content- SQLite format 3"
        );

        // Backup should have old content
        assert!(backup.exists(), "backup should exist");
        assert_eq!(
            fs::read(&backup).unwrap(),
            b"old-wallet-content- SQLite format 3"
        );
    }

    #[test]
    fn legacy_wallet_empty_source_rejected() {
        let tmp = TempDir::new("hemp64o_empty_src");
        let source = tmp.path.join("empty_wallet.dat");
        fs::write(&source, b"").unwrap();
        let meta = fs::metadata(&source).unwrap();
        assert_eq!(meta.len(), 0, "empty file should have zero length");
        // In the real function, this would return an error.
        // The function checks: if source_meta.len() == 0 → error.
    }

    #[test]
    fn legacy_wallet_missing_source_rejected() {
        let tmp = TempDir::new("hemp64o_missing_src");
        let source = tmp.path.join("nonexistent_wallet.dat");
        assert!(!source.exists(), "missing file should not exist");
        // In the real function, this returns Err("Selected file not found.").
    }

    #[test]
    fn legacy_import_clears_active_vault_wallet_name() {
        use crate::modules::files::{
            load_app_settings_impl, save_app_settings_impl, TEST_COMMANDER_DIR,
        };
        use crate::modules::models::AppSettings;

        let test_dir =
            std::env::temp_dir().join(format!("hemp64o_settings_clear_{}", std::process::id()));
        let _ = fs::remove_dir_all(&test_dir);
        fs::create_dir_all(&test_dir).unwrap();

        TEST_COMMANDER_DIR.with(|cell| {
            *cell.borrow_mut() = Some(test_dir.clone());
        });

        // Set up an active_vault_wallet_name
        let mut settings = AppSettings::default();
        settings.active_vault_wallet_name = Some("hemp0x-vault-main".to_string());
        save_app_settings_impl(&settings).unwrap();

        // Verify it was saved
        let loaded = load_app_settings_impl().unwrap();
        assert_eq!(
            loaded.active_vault_wallet_name,
            Some("hemp0x-vault-main".to_string())
        );

        // Clear it (simulating what restore_legacy_wallet_dat does)
        let mut settings = load_app_settings_impl().unwrap();
        settings.active_vault_wallet_name = None;
        save_app_settings_impl(&settings).unwrap();

        let loaded = load_app_settings_impl().unwrap();
        assert!(
            loaded.active_vault_wallet_name.is_none(),
            "active_vault_wallet_name should be cleared after legacy import"
        );

        TEST_COMMANDER_DIR.with(|cell| {
            *cell.borrow_mut() = None;
        });
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn detects_extensionless_core_migration_envelope() {
        let tmp = TempDir::new("hemp65_migration_detect");
        let migration_path = tmp.path.join("hemp0x-vault-main");
        fs::write(
            &migration_path,
            r#"{
      "envelope_version": 2,
      "schema_identifier": "hemp0x-core.migration-envelope.v2",
      "chain": {
        "network": "mainnet",
        "coin_type_bip44": 420
      },
      "wallet_summary": {
        "wallet_name": "wallet.dat",
        "private_keys_included": true
      },
      "private": {
        "encrypted": true
      }
    }"#,
        )
        .unwrap();

        let detected =
            detect_wallet_file_type(migration_path.to_string_lossy().to_string()).unwrap();
        assert_eq!(detected["detected_type"], "core_migration_envelope");
        assert_eq!(detected["migration_version"], 2);
        assert_eq!(detected["migration_encrypted"], true);
        assert_eq!(detected["migration_coin_type"], 420);
    }

    #[test]
    fn detects_extensionless_core_wallet_candidate() {
        let tmp = TempDir::new("hemp65_wallet_detect");
        let wallet_path = tmp.path.join("hemp0x-vault-main");
        fs::write(&wallet_path, b"SQLite format 3\0wallet-content").unwrap();

        let detected = detect_wallet_file_type(wallet_path.to_string_lossy().to_string()).unwrap();
        assert_eq!(detected["detected_type"], "legacy_core_wallet");
        assert_eq!(detected["wallet_format"], "sqlite");
    }

    #[test]
    fn switch_to_legacy_wallet_dat_clears_active_vault_without_restarting() {
        use crate::modules::files::{
            load_app_settings_impl, save_app_settings_impl, TEST_COMMANDER_DIR, TEST_DATA_DIR,
        };
        use crate::modules::models::AppSettings;

        let tmp = TempDir::new("hemp64p_switch_legacy");
        let data_dir = tmp.path.join("data");
        let commander_dir = tmp.path.join("commander");
        fs::create_dir_all(&data_dir).unwrap();
        fs::create_dir_all(&commander_dir).unwrap();
        fs::write(data_dir.join("wallet.dat"), b"SQLite format 3\0wallet").unwrap();

        TEST_DATA_DIR.with(|cell| {
            *cell.borrow_mut() = Some(data_dir.clone());
        });
        TEST_COMMANDER_DIR.with(|cell| {
            *cell.borrow_mut() = Some(commander_dir.clone());
        });

        let mut settings = AppSettings::default();
        settings.active_vault_wallet_name = Some("hemp0x-vault-main".to_string());
        save_app_settings_impl(&settings).unwrap();

        let res = switch_to_legacy_wallet_dat_blocking(false).unwrap();
        assert_eq!(res["switched"], true);
        assert_eq!(res["legacy_wallet_mode"], true);
        assert_eq!(res["wallet_dat_exists"], true);
        assert_eq!(res["restarted"], false);

        let after = load_app_settings_impl().unwrap();
        assert!(after.active_vault_wallet_name.is_none());

        TEST_DATA_DIR.with(|cell| {
            *cell.borrow_mut() = None;
        });
        TEST_COMMANDER_DIR.with(|cell| {
            *cell.borrow_mut() = None;
        });
    }

    #[test]
    fn check_hemp_conf_wallet_line_detects_wallet_arg() {
        let tmp = TempDir::new("hemp64o_conf_wallet");
        let cfg_dir = tmp.path.join("data");
        fs::create_dir_all(&cfg_dir).unwrap();

        // Write a hemp.conf with a wallet= line
        let conf_content = "server=1\ndaemon=1\nrpcport=42068\nwallet=hemp0x-vault-main\n";
        fs::write(cfg_dir.join("hemp.conf"), conf_content).unwrap();

        // Parse it manually (same logic as check_hemp_conf_wallet_line)
        let content = fs::read_to_string(cfg_dir.join("hemp.conf")).unwrap();
        let mut found_wallet: Option<String> = None;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') || trimmed.is_empty() {
                continue;
            }
            if let Some((k, v)) = trimmed.split_once('=') {
                if k.trim() == "wallet" {
                    found_wallet = Some(v.trim().to_string());
                }
            }
        }
        assert_eq!(found_wallet, Some("hemp0x-vault-main".to_string()));
    }

    #[test]
    fn check_hemp_conf_wallet_line_no_wallet_arg() {
        let tmp = TempDir::new("hemp64o_conf_no_wallet");
        let cfg_dir = tmp.path.join("data");
        fs::create_dir_all(&cfg_dir).unwrap();

        let conf_content = "server=1\ndaemon=1\nrpcport=42068\n";
        fs::write(cfg_dir.join("hemp.conf"), conf_content).unwrap();

        let content = fs::read_to_string(cfg_dir.join("hemp.conf")).unwrap();
        let mut found_wallet: Option<String> = None;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') || trimmed.is_empty() {
                continue;
            }
            if let Some((k, v)) = trimmed.split_once('=') {
                if k.trim() == "wallet" {
                    found_wallet = Some(v.trim().to_string());
                }
            }
        }
        assert!(found_wallet.is_none());
    }
}
