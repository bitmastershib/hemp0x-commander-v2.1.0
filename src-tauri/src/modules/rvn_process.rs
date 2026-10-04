// Ravencoin daemon process management for Commander multi-chain support.
//
// Manages the ravend sidecar: start, stop, health checks, and process
// detection. Mirrors the hemp0x process.rs patterns.
//
// Copyright (c) 2024-2025 The Hemp0x developers
// Distributed under the MIT software license.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::modules::rvn_models::{RvnBinaryStatus, RvnRuntimeStatus};
use crate::modules::rvn_rpc::{call_rvn_rpc, ensure_rvn_config, rvn_data_dir};
use crate::modules::utils::bin_name;

// ─── Process Detection ───────────────────────────────────────────────────────

fn rvn_daemon_process_running() -> bool {
    #[cfg(unix)]
    {
        return Command::new("ps")
            .arg("-eo")
            .arg("stat=,comm=")
            .output()
            .map(|output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .any(|line| {
                            let mut parts = line.split_whitespace();
                            let stat = parts.next().unwrap_or("");
                            let command = parts.next().unwrap_or("");
                            command == "ravend" && !stat.starts_with('Z')
                        })
            })
            .unwrap_or(false);
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        return Command::new("tasklist")
            .creation_flags(0x08000000)
            .arg("/FI")
            .arg("IMAGENAME eq ravend.exe")
            .arg("/NH")
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains("ravend.exe"))
            .unwrap_or(false);
    }

    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

fn rvn_node_process_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn wait_for_rvn_daemon_exit(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while rvn_daemon_process_running() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(250));
    }
    !rvn_daemon_process_running()
}

// ─── Binary Resolution ──────────────────────────────────────────────────────

fn resolve_rvn_bin(name: &str) -> String {
    // Check alongside Commander's bundled binaries first
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(bin_name(name));
            if candidate.exists() {
                return candidate.to_string_lossy().to_string();
            }
            let resources = dir.join("resources").join(bin_name(name));
            if resources.exists() {
                return resources.to_string_lossy().to_string();
            }
        }
    }

    // Check in current working directory tree
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join(bin_name(name));
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
        let binaries = cwd.join("binaries").join(bin_name(name));
        if binaries.exists() {
            return binaries.to_string_lossy().to_string();
        }
    }

    // Fallback: return just the name (will fail at spawn if not in PATH)
    name.to_string()
}

// ─── Tauri Commands ──────────────────────────────────────────────────────────

#[tauri::command]
pub async fn rvn_start_node() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(rvn_start_node_blocking)
        .await
        .map_err(|e| format!("RVN start node task failed: {e}"))?
}

fn rvn_start_node_blocking() -> Result<String, String> {
    let _guard = rvn_node_process_lock()
        .lock()
        .map_err(|_| "RVN node process lock is poisoned".to_string())?;

    // If ravend is already running, check if it responds
    if rvn_daemon_process_running() {
        if call_rvn_rpc("getblockchaininfo", &[]).is_ok() {
            return Ok("Ravencoin node is already running.".to_string());
        }
        // Running but not responding yet — wait briefly to see if it comes up quickly
        let deadline = Instant::now() + Duration::from_secs(15);
        while rvn_daemon_process_running() && Instant::now() < deadline {
            if call_rvn_rpc("getblockchaininfo", &[]).is_ok() {
                return Ok("Ravencoin node is already running.".to_string());
            }
            thread::sleep(Duration::from_millis(1500));
        }
        
        // If it's still running but not answering RPC, it's probably just verifying blocks.
        if rvn_daemon_process_running() {
            return Ok("Ravencoin node is running but still starting up (verifying blocks).".to_string());
        }
    }

    // Ensure config exists
    let config_path = ensure_rvn_config()?;
    let data_dir = rvn_data_dir()?;

    // Clean up stale runtime files
    if !rvn_daemon_process_running() {
        for name in [".cookie", ".lock"] {
            let path = data_dir.join(name);
            if path.exists() && path.is_file() {
                let _ = fs::remove_file(&path);
            }
        }
    }

    // Resolve ravend binary
    let daemon = resolve_rvn_bin("ravend");
    let daemon_path = PathBuf::from(&daemon);
    if !daemon_path.exists() {
        return Err(format!(
            "Ravencoin daemon not found at {}. Please place ravend in the Commander binaries folder.",
            daemon
        ));
    }

    // Spawn ravend
    let mut cmd = Command::new(&daemon);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    cmd.arg(format!("-conf={}", config_path.to_string_lossy()))
        .arg(format!("-datadir={}", data_dir.to_string_lossy()));

    #[cfg(unix)]
    {
        cmd.arg("-daemon");
    }

    cmd.stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to start ravend: {e}"))?;

    Ok("Ravencoin node started.".to_string())
}

#[tauri::command]
pub async fn rvn_stop_node() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(rvn_stop_node_blocking)
        .await
        .map_err(|e| format!("RVN stop node task failed: {e}"))?
}

fn rvn_stop_node_blocking() -> Result<String, String> {
    let _guard = rvn_node_process_lock()
        .lock()
        .map_err(|_| "RVN node process lock is poisoned".to_string())?;

    if !rvn_daemon_process_running() {
        return Ok("Ravencoin node is not running.".to_string());
    }

    match call_rvn_rpc("stop", &[]) {
        Ok(_) => {
            // Wait for process to exit
            wait_for_rvn_daemon_exit(Duration::from_secs(30));
            Ok("Ravencoin node stopping.".to_string())
        }
        Err(_e) if !rvn_daemon_process_running() => {
            Ok("Ravencoin node already stopped.".to_string())
        }
        Err(e) => {
            // Force kill if RPC fails
            if rvn_daemon_process_running() {
                #[cfg(unix)]
                let _ = std::process::Command::new("killall").arg("-9").arg("ravend").output();
                #[cfg(windows)]
                let _ = std::process::Command::new("taskkill").args(&["/F", "/IM", "ravend.exe"]).output();
                
                thread::sleep(Duration::from_secs(2));
                if rvn_daemon_process_running() {
                    return Err(format!("Failed to stop Ravencoin node gracefully, and force kill failed: {e}"));
                }
            }
            Ok("Ravencoin node forcefully stopped.".to_string())
        }
    }
}

#[tauri::command]
pub async fn rvn_stop_node_and_wait(timeout_ms: Option<u64>) -> Result<String, String> {
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(90_000).max(1_000));
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = rvn_node_process_lock()
            .lock()
            .map_err(|_| "RVN node process lock is poisoned".to_string())?;

        if !rvn_daemon_process_running() {
            return Ok("Ravencoin node is not running.".to_string());
        }

        let _ = call_rvn_rpc("stop", &[]);

        if wait_for_rvn_daemon_exit(timeout) {
            // Clean up stale files
            if let Ok(dir) = rvn_data_dir() {
                for name in [".cookie", ".lock"] {
                    let path = dir.join(name);
                    if path.exists() && path.is_file() {
                        let _ = fs::remove_file(&path);
                    }
                }
            }
            Ok("Ravencoin node stopped.".to_string())
        } else {
            Err("Ravencoin node did not stop within the timeout.".to_string())
        }
    })
    .await
    .map_err(|e| format!("RVN stop-and-wait task failed: {e}"))?
}

#[tauri::command]
pub async fn rvn_get_runtime_status() -> RvnRuntimeStatus {
    tauri::async_runtime::spawn_blocking(|| {
        let running = rvn_daemon_process_running();
        let rpc_ready = if running {
            call_rvn_rpc("getblockchaininfo", &[]).is_ok()
        } else {
            false
        };

        let version = if rpc_ready {
            call_rvn_rpc("getnetworkinfo", &[])
                .ok()
                .and_then(|info| info["subversion"].as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "--".to_string())
        } else {
            "--".to_string()
        };

        let data_dir = rvn_data_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let config_exists = rvn_data_dir()
            .ok()
            .map(|d| d.join("raven.conf").exists())
            .unwrap_or(false);

        RvnRuntimeStatus {
            running,
            rpc_ready,
            version,
            data_dir,
            config_exists,
        }
    })
    .await
    .unwrap_or_else(|_| RvnRuntimeStatus {
        running: false,
        rpc_ready: false,
        version: "--".to_string(),
        data_dir: "".to_string(),
        config_exists: false,
    })
}

#[tauri::command]
pub async fn rvn_get_binary_status() -> RvnBinaryStatus {
    tauri::async_runtime::spawn_blocking(|| {
        let daemon_path = resolve_rvn_bin("ravend");
        let cli_path = resolve_rvn_bin("raven-cli");

        RvnBinaryStatus {
            daemon_exists: PathBuf::from(&daemon_path).exists(),
            cli_exists: PathBuf::from(&cli_path).exists(),
            daemon_path,
            cli_path,
        }
    })
    .await
    .unwrap_or_else(|_| RvnBinaryStatus {
        daemon_exists: false,
        cli_exists: false,
        daemon_path: "".to_string(),
        cli_path: "".to_string(),
    })
}

#[tauri::command]
pub async fn rvn_wait_for_daemon_ready(timeout_ms: Option<u64>) -> Result<String, String> {
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(60_000).max(1_000));
    tauri::async_runtime::spawn_blocking(move || {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(info) = call_rvn_rpc("getblockchaininfo", &[]) {
                if info.get("blocks").is_some() {
                    return Ok("Ravencoin daemon is ready.".to_string());
                }
            }
            thread::sleep(Duration::from_millis(750));
        }
        Err("Ravencoin daemon did not become ready within the timeout.".to_string())
    })
    .await
    .map_err(|e| format!("RVN wait task failed: {e}"))?
}
