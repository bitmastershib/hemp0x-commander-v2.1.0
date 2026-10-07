// Blockchain snapshot ("Fast Sync") support for the Hemp0x and Ravencoin nodes.
//
// Flow driven by the frontend:
//   1. `snapshot_get_info`  - read the published manifest and report what is available.
//   2. `snapshot_download`  - resumable download + SHA-256 verification. The node keeps
//                             running (and syncing) while this happens.
//   3. frontend stops the node and waits for the process to exit.
//   4. `snapshot_install`   - extract into a temp folder, swap the chain folders with
//                             rollback on failure, then clean up.
//   5. frontend restarts the node.
//
// Snapshots only contain chain data folders (blocks, chainstate, assets, ...). Wallet
// files, configs and peer data are never touched.
//
// Copyright (c) 2024-2026 The Hemp0x developers
// Distributed under the MIT software license.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};

use crate::modules::files::data_dir as hemp_data_dir;
use crate::modules::rvn_rpc::rvn_data_dir;

// ─── Configuration ───────────────────────────────────────────────────────────

/// Published snapshot manifests, tried in order. Each manifest is a small JSON file:
///
/// ```json
/// {
///   "hemp0x":    { "url": "https://.../hemp0x-snapshot.7z", "sha256": "...", "size": 123, "height": 456789, "date": "2026-10-06" },
///   "ravencoin": { "url": "https://.../ravencoin-snapshot.zip", "sha256": "...", "size": 123, "height": 3456789, "date": "2026-10-06" }
/// }
/// ```
///
/// Generate it with `scripts/create-snapshot.ps1`.
pub const SNAPSHOT_MANIFEST_URLS: &[&str] = &[
    "https://github.com/bitmastershib/hemp0x-commander-v2.1.0/releases/download/snapshots/manifest.json",
    "https://hemp0x.com/snapshots/manifest.json"
];

/// Optional override for testing (e.g. `http://127.0.0.1:8000/manifest.json`).
const MANIFEST_URL_ENV: &str = "HEMP0X_SNAPSHOT_MANIFEST_URL";

/// Chain database folders that a snapshot may replace. Anything else in the data
/// directory (wallets, configs, peers, logs) is left untouched.
const CHAIN_DIRS: &[&str] = &[
    "blocks",
    "chainstate",
    "assets",
    "messages",
    "restricted",
    "rewards",
    "indexes",
];
const REQUIRED_DIRS: &[&str] = &["blocks", "chainstate"];

const DOWNLOAD_DIR: &str = "_snapshot_download";
const EXTRACT_DIR: &str = "_snapshot_temp";
const BACKUP_DIR: &str = "_snapshot_previous";

const PROGRESS_EVENT: &str = "snapshot-progress";
const IO_BUFFER: usize = 1 << 20; // 1 MiB
const MAX_DOWNLOAD_ATTEMPTS: u32 = 6;

// ─── Chain selection & per-chain state ──────────────────────────────────────

struct ChainFlags {
    busy: AtomicBool,
    cancel: AtomicBool,
}

static HEMP_FLAGS: ChainFlags = ChainFlags {
    busy: AtomicBool::new(false),
    cancel: AtomicBool::new(false),
};
static RVN_FLAGS: ChainFlags = ChainFlags {
    busy: AtomicBool::new(false),
    cancel: AtomicBool::new(false),
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Chain {
    Hemp0x,
    Ravencoin,
}

impl Chain {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "hemp0x" | "hemp" => Ok(Self::Hemp0x),
            "ravencoin" | "rvn" => Ok(Self::Ravencoin),
            other => Err(format!("Unknown chain '{other}'")),
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Hemp0x => "hemp0x",
            Self::Ravencoin => "ravencoin",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Hemp0x => "Hemp0x",
            Self::Ravencoin => "Ravencoin",
        }
    }

    fn data_dir(self) -> Result<PathBuf, String> {
        match self {
            Self::Hemp0x => hemp_data_dir(),
            Self::Ravencoin => rvn_data_dir(),
        }
    }

    fn flags(self) -> &'static ChainFlags {
        match self {
            Self::Hemp0x => &HEMP_FLAGS,
            Self::Ravencoin => &RVN_FLAGS,
        }
    }

    fn daemon_running(self) -> bool {
        match self {
            Self::Hemp0x => crate::modules::process::daemon_process_running(),
            Self::Ravencoin => crate::modules::rvn_process::rvn_daemon_process_running(),
        }
    }
}

/// Marks a chain as busy for the lifetime of the guard so two snapshot operations
/// can never run against the same data directory at once.
struct BusyGuard(&'static ChainFlags);

impl BusyGuard {
    fn acquire(chain: Chain) -> Result<Self, String> {
        let flags = chain.flags();
        if flags.busy.swap(true, Ordering::SeqCst) {
            return Err(format!(
                "A {} snapshot operation is already running.",
                chain.label()
            ));
        }
        flags.cancel.store(false, Ordering::SeqCst);
        Ok(Self(flags))
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::SeqCst);
    }
}

fn cancelled(chain: Chain) -> bool {
    chain.flags().cancel.load(Ordering::SeqCst)
}

// ─── Manifest ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
    /// Archive size in bytes.
    #[serde(default)]
    pub size: Option<u64>,
    /// Total uncompressed size in bytes (used for disk-space hints).
    #[serde(default)]
    pub extracted_size: Option<u64>,
    /// Block height the snapshot was taken at.
    #[serde(default)]
    pub height: Option<u64>,
    #[serde(default)]
    pub date: Option<String>,
    /// "7z" or "zip". Inferred from the URL when omitted.
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    hemp0x: Option<SnapshotEntry>,
    #[serde(default)]
    ravencoin: Option<SnapshotEntry>,
}

fn manifest_urls() -> Vec<String> {
    if let Ok(url) = std::env::var(MANIFEST_URL_ENV) {
        if !url.trim().is_empty() {
            return vec![url.trim().to_string()];
        }
    }
    SNAPSHOT_MANIFEST_URLS.iter().map(|s| s.to_string()).collect()
}

fn fetch_entry(chain: Chain) -> Result<(SnapshotEntry, String), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build();

    let mut errors = Vec::new();
    for url in manifest_urls() {
        let response = match agent.get(&url).call() {
            Ok(r) => r,
            Err(e) => {
                errors.push(format!("{url}: {e}"));
                continue;
            }
        };
        let manifest: Manifest = match response.into_json() {
            Ok(m) => m,
            Err(e) => {
                errors.push(format!("{url}: invalid manifest ({e})"));
                continue;
            }
        };
        let entry = match chain {
            Chain::Hemp0x => manifest.hemp0x,
            Chain::Ravencoin => manifest.ravencoin,
        };
        return match entry {
            Some(e) if !e.url.trim().is_empty() => Ok((e, url)),
            _ => Err(format!(
                "No {} snapshot is published yet.",
                chain.label()
            )),
        };
    }
    Err(format!(
        "Snapshot server unreachable. {}",
        errors.join(" | ")
    ))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArchiveFormat {
    SevenZ,
    Zip,
}

impl ArchiveFormat {
    fn detect(entry: &SnapshotEntry) -> Result<Self, String> {
        let hint = entry
            .format
            .clone()
            .unwrap_or_else(|| entry.url.split('?').next().unwrap_or("").to_string())
            .to_ascii_lowercase();
        if hint == "7z" || hint.ends_with(".7z") {
            Ok(Self::SevenZ)
        } else if hint == "zip" || hint.ends_with(".zip") {
            Ok(Self::Zip)
        } else {
            Err("Unsupported snapshot format (expected .7z or .zip).".to_string())
        }
    }

    fn ext(self) -> &'static str {
        match self {
            Self::SevenZ => "7z",
            Self::Zip => "zip",
        }
    }
}

/// Identifies a specific published snapshot so a stale local download is never
/// mistaken for a newer one.
fn entry_fingerprint(entry: &SnapshotEntry) -> String {
    match &entry.sha256 {
        Some(sha) if !sha.trim().is_empty() => sha.trim().to_ascii_lowercase(),
        _ => format!("{}|{}", entry.url, entry.size.unwrap_or(0)),
    }
}

struct DownloadPaths {
    dir: PathBuf,
    archive: PathBuf,
    part: PathBuf,
    part_source: PathBuf,
    verified_marker: PathBuf,
}

fn download_paths(chain: Chain, format: ArchiveFormat) -> Result<DownloadPaths, String> {
    let dir = chain.data_dir()?.join(DOWNLOAD_DIR);
    let name = format!("{}-snapshot.{}", chain.key(), format.ext());
    Ok(DownloadPaths {
        archive: dir.join(&name),
        part: dir.join(format!("{name}.part")),
        part_source: dir.join(format!("{name}.part.source")),
        verified_marker: dir.join(format!("{name}.verified")),
        dir,
    })
}

fn read_marker(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Finds an already downloaded + verified archive regardless of which format it was.
fn find_verified_archive(chain: Chain) -> Option<(PathBuf, ArchiveFormat)> {
    for format in [ArchiveFormat::SevenZ, ArchiveFormat::Zip] {
        if let Ok(paths) = download_paths(chain, format) {
            if paths.archive.is_file() && paths.verified_marker.is_file() {
                return Some((paths.archive, format));
            }
        }
    }
    None
}

// ─── Progress reporting ──────────────────────────────────────────────────────

#[derive(Clone, Serialize)]
struct ProgressPayload {
    chain: String,
    stage: String,
    status: String,
    /// Stage progress, 0-100.
    progress: f64,
    done_bytes: u64,
    total_bytes: u64,
    speed_bps: f64,
    eta_secs: Option<u64>,
}

struct Reporter<'a> {
    app: &'a AppHandle,
    chain: Chain,
    stage: &'static str,
    started: Instant,
    last_emit: Option<Instant>,
    session_base: u64,
}

impl<'a> Reporter<'a> {
    fn new(app: &'a AppHandle, chain: Chain, stage: &'static str) -> Self {
        Self {
            app,
            chain,
            stage,
            started: Instant::now(),
            last_emit: None,
            session_base: 0,
        }
    }

    fn message(&self, status: impl Into<String>, progress: f64) {
        let _ = self.app.emit(
            PROGRESS_EVENT,
            ProgressPayload {
                chain: self.chain.key().to_string(),
                stage: self.stage.to_string(),
                status: status.into(),
                progress: progress.clamp(0.0, 100.0),
                done_bytes: 0,
                total_bytes: 0,
                speed_bps: 0.0,
                eta_secs: None,
            },
        );
    }

    /// Emits byte progress, throttled to ~3 updates per second unless `force`.
    fn bytes(&mut self, verb: &str, done: u64, total: u64, force: bool) {
        let now = Instant::now();
        if !force {
            if let Some(last) = self.last_emit {
                if now.duration_since(last) < Duration::from_millis(350) {
                    return;
                }
            }
        }
        self.last_emit = Some(now);

        let elapsed = now.duration_since(self.started).as_secs_f64().max(0.001);
        let session_bytes = done.saturating_sub(self.session_base);
        let speed = session_bytes as f64 / elapsed;
        let eta = if total > done && speed > 1.0 {
            Some(((total - done) as f64 / speed) as u64)
        } else {
            None
        };
        let progress = if total > 0 {
            done as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        let status = if total > 0 {
            format!("{verb} {} of {}", human_bytes(done), human_bytes(total))
        } else {
            format!("{verb} {}", human_bytes(done))
        };

        let _ = self.app.emit(
            PROGRESS_EVENT,
            ProgressPayload {
                chain: self.chain.key().to_string(),
                stage: self.stage.to_string(),
                status,
                progress: progress.clamp(0.0, 100.0),
                done_bytes: done,
                total_bytes: total,
                speed_bps: speed,
                eta_secs: eta,
            },
        );
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

// ─── Tauri commands ──────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct SnapshotInfo {
    chain: String,
    available: bool,
    entry: Option<SnapshotEntry>,
    /// A verified archive is already on disk and ready to install.
    downloaded: bool,
    /// Bytes of an interrupted download that can be resumed.
    partial_bytes: u64,
    busy: bool,
    error: Option<String>,
    manifest_url: Option<String>,
}

#[tauri::command]
pub async fn snapshot_get_info(chain: String) -> Result<SnapshotInfo, String> {
    let chain = Chain::parse(&chain)?;
    tauri::async_runtime::spawn_blocking(move || {
        let busy = chain.flags().busy.load(Ordering::SeqCst);
        let downloaded_local = find_verified_archive(chain).is_some();

        match fetch_entry(chain) {
            Ok((entry, manifest_url)) => {
                let mut downloaded = false;
                let mut partial_bytes = 0;
                if let Ok(format) = ArchiveFormat::detect(&entry) {
                    if let Ok(paths) = download_paths(chain, format) {
                        let fingerprint = entry_fingerprint(&entry);
                        downloaded = paths.archive.is_file()
                            && read_marker(&paths.verified_marker).as_deref()
                                == Some(fingerprint.as_str());
                        if read_marker(&paths.part_source).as_deref() == Some(fingerprint.as_str())
                        {
                            partial_bytes = fs::metadata(&paths.part).map(|m| m.len()).unwrap_or(0);
                        }
                    }
                }
                Ok(SnapshotInfo {
                    chain: chain.key().to_string(),
                    available: true,
                    entry: Some(entry),
                    downloaded,
                    partial_bytes,
                    busy,
                    error: None,
                    manifest_url: Some(manifest_url),
                })
            }
            Err(error) => Ok(SnapshotInfo {
                chain: chain.key().to_string(),
                // An archive that was already downloaded can still be installed offline.
                available: downloaded_local,
                entry: None,
                downloaded: downloaded_local,
                partial_bytes: 0,
                busy,
                error: Some(error),
                manifest_url: None,
            }),
        }
    })
    .await
    .map_err(|e| format!("Snapshot info task failed: {e}"))?
}

#[tauri::command]
pub async fn snapshot_download(app_handle: AppHandle, chain: String) -> Result<String, String> {
    let chain = Chain::parse(&chain)?;
    tauri::async_runtime::spawn_blocking(move || download_blocking(&app_handle, chain))
        .await
        .map_err(|e| format!("Snapshot download task failed: {e}"))?
}

#[tauri::command]
pub async fn snapshot_install(app_handle: AppHandle, chain: String) -> Result<String, String> {
    let chain = Chain::parse(&chain)?;
    tauri::async_runtime::spawn_blocking(move || install_blocking(&app_handle, chain))
        .await
        .map_err(|e| format!("Snapshot install task failed: {e}"))?
}

#[tauri::command]
pub fn snapshot_cancel(chain: String) -> Result<(), String> {
    let chain = Chain::parse(&chain)?;
    chain.flags().cancel.store(true, Ordering::SeqCst);
    Ok(())
}

/// Removes any downloaded or partial archive for the chain (frees disk space).
#[tauri::command]
pub async fn snapshot_discard(chain: String) -> Result<(), String> {
    let chain = Chain::parse(&chain)?;
    tauri::async_runtime::spawn_blocking(move || {
        if chain.flags().busy.load(Ordering::SeqCst) {
            return Err("Cannot discard while a snapshot operation is running.".to_string());
        }
        let dir = chain.data_dir()?.join(DOWNLOAD_DIR);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| format!("Failed to remove {}: {e}", dir.display()))?;
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("Snapshot discard task failed: {e}"))?
}

// ─── Download ────────────────────────────────────────────────────────────────

enum DownloadError {
    /// Transient network problem; the partial file is kept and we retry/resume.
    Retry(String),
    Fatal(String),
}

fn download_blocking(app: &AppHandle, chain: Chain) -> Result<String, String> {
    let _guard = BusyGuard::acquire(chain)?;
    let mut reporter = Reporter::new(app, chain, "download");
    reporter.message("Contacting snapshot server...", 0.0);

    let (entry, _) = fetch_entry(chain)?;
    let format = ArchiveFormat::detect(&entry)?;
    let paths = download_paths(chain, format)?;
    let fingerprint = entry_fingerprint(&entry);
    fs::create_dir_all(&paths.dir)
        .map_err(|e| format!("Failed to create {}: {e}", paths.dir.display()))?;

    // Already downloaded and verified for this exact manifest entry.
    if paths.archive.is_file()
        && read_marker(&paths.verified_marker).as_deref() == Some(fingerprint.as_str())
    {
        reporter.message("Snapshot already downloaded and verified.", 100.0);
        return Ok("Snapshot already downloaded.".to_string());
    }

    // Drop anything left over from an older published snapshot.
    let _ = fs::remove_file(&paths.archive);
    let _ = fs::remove_file(&paths.verified_marker);
    if read_marker(&paths.part_source).as_deref() != Some(fingerprint.as_str()) {
        let _ = fs::remove_file(&paths.part);
    }
    fs::write(&paths.part_source, &fingerprint)
        .map_err(|e| format!("Failed to write download marker: {e}"))?;

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        // Large downloads must not be capped by reqwest's default 30s total timeout.
        .timeout(None::<Duration>)
        .user_agent(concat!("Hemp0x-Commander/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let mut attempt = 0;
    loop {
        attempt += 1;
        match download_once(&client, &entry, &paths, chain, &mut reporter) {
            Ok(()) => break,
            Err(DownloadError::Fatal(e)) => return Err(e),
            Err(DownloadError::Retry(e)) => {
                if cancelled(chain) {
                    return Err("Download paused. Click Fast Sync again to resume.".to_string());
                }
                if attempt >= MAX_DOWNLOAD_ATTEMPTS {
                    return Err(format!(
                        "{e}. Progress was saved - click Fast Sync again to resume."
                    ));
                }
                let wait = Duration::from_secs(3 * attempt as u64);
                reporter.message(
                    format!(
                        "Connection dropped ({e}). Resuming in {}s (attempt {}/{})...",
                        wait.as_secs(),
                        attempt + 1,
                        MAX_DOWNLOAD_ATTEMPTS
                    ),
                    0.0,
                );
                std::thread::sleep(wait);
            }
        }
    }

    verify_download(&entry, &paths, chain, app)?;

    fs::rename(&paths.part, &paths.archive)
        .map_err(|e| format!("Failed to finalize download: {e}"))?;
    fs::write(&paths.verified_marker, &fingerprint)
        .map_err(|e| format!("Failed to write verification marker: {e}"))?;
    let _ = fs::remove_file(&paths.part_source);

    Reporter::new(app, chain, "download").message("Download complete and verified.", 100.0);
    Ok("Snapshot downloaded and verified.".to_string())
}

fn download_once(
    client: &reqwest::blocking::Client,
    entry: &SnapshotEntry,
    paths: &DownloadPaths,
    chain: Chain,
    reporter: &mut Reporter,
) -> Result<(), DownloadError> {
    let existing = fs::metadata(&paths.part).map(|m| m.len()).unwrap_or(0);

    // Previous attempt already finished the transfer.
    if let Some(size) = entry.size {
        if size > 0 && existing == size {
            return Ok(());
        }
        if existing > size {
            let _ = fs::remove_file(&paths.part);
            return Err(DownloadError::Retry("Partial file was larger than expected".into()));
        }
    }

    let mut request = client.get(&entry.url);
    if existing > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={existing}-"));
    }
    let mut response = request
        .send()
        .map_err(|e| DownloadError::Retry(format!("Could not reach snapshot server: {e}")))?;

    let status = response.status();
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        // Server says there is nothing past our offset - treat as finished and let
        // verification decide.
        return Ok(());
    }
    if !status.is_success() {
        let msg = format!("Snapshot server returned {status}");
        return Err(if status.is_server_error() {
            DownloadError::Retry(msg)
        } else {
            DownloadError::Fatal(msg)
        });
    }

    let resuming = existing > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;
    let mut done = if resuming { existing } else { 0 };
    let total = match response.content_length() {
        Some(len) if resuming => existing + len,
        Some(len) => len,
        None => entry.size.unwrap_or(0),
    };

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(resuming)
        .truncate(!resuming)
        .open(&paths.part)
        .map_err(|e| DownloadError::Fatal(format!("Failed to open download file: {e}")))?;
    let mut writer = BufWriter::with_capacity(IO_BUFFER, file);
    let mut buffer = vec![0u8; IO_BUFFER];

    reporter.session_base = done;
    reporter.started = Instant::now();
    reporter.bytes(
        if resuming { "Resuming download:" } else { "Downloading" },
        done,
        total,
        true,
    );

    loop {
        if cancelled(chain) {
            let _ = writer.flush();
            return Err(DownloadError::Fatal(
                "Download paused. Click Fast Sync again to resume.".to_string(),
            ));
        }
        let read = match response.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = writer.flush();
                return Err(DownloadError::Retry(format!("Download interrupted: {e}")));
            }
        };
        writer.write_all(&buffer[..read]).map_err(|e| {
            DownloadError::Fatal(format!(
                "Failed to write snapshot to disk (is the drive full?): {e}"
            ))
        })?;
        done += read as u64;
        reporter.bytes("Downloading", done, total, false);
    }

    writer
        .flush()
        .map_err(|e| DownloadError::Fatal(format!("Failed to flush download: {e}")))?;
    if let Ok(file) = writer.into_inner() {
        let _ = file.sync_all();
    }
    reporter.bytes("Downloading", done, total, true);

    if total > 0 && done < total {
        return Err(DownloadError::Retry(format!(
            "Connection closed early ({} of {})",
            human_bytes(done),
            human_bytes(total)
        )));
    }
    Ok(())
}

fn verify_download(
    entry: &SnapshotEntry,
    paths: &DownloadPaths,
    chain: Chain,
    app: &AppHandle,
) -> Result<(), String> {
    let actual_size = fs::metadata(&paths.part)
        .map(|m| m.len())
        .map_err(|e| format!("Downloaded file missing: {e}"))?;

    if let Some(expected) = entry.size {
        if expected > 0 && actual_size != expected {
            let _ = fs::remove_file(&paths.part);
            return Err(format!(
                "Downloaded size {} does not match expected {}. The file was removed - please try again.",
                actual_size, expected
            ));
        }
    }

    let Some(expected_sha) = entry
        .sha256
        .as_ref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
    else {
        return Ok(());
    };

    let mut reporter = Reporter::new(app, chain, "verify");
    let file = File::open(&paths.part).map_err(|e| format!("Failed to open download: {e}"))?;
    let mut reader = BufReader::with_capacity(IO_BUFFER, file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; IO_BUFFER];
    let mut done = 0u64;
    loop {
        if cancelled(chain) {
            return Err("Verification cancelled. The download is kept - click Fast Sync to continue.".into());
        }
        let n = reader
            .read(&mut buffer)
            .map_err(|e| format!("Failed to read download for verification: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        done += n as u64;
        reporter.bytes("Verifying", done, actual_size, false);
    }
    reporter.bytes("Verifying", done, actual_size, true);

    let actual_sha = hex::encode(hasher.finalize());
    if actual_sha != expected_sha {
        let _ = fs::remove_file(&paths.part);
        return Err(
            "Snapshot checksum mismatch - the download was corrupted or tampered with. It has been deleted; please try again."
                .to_string(),
        );
    }
    Ok(())
}

// ─── Install ─────────────────────────────────────────────────────────────────

fn install_blocking(app: &AppHandle, chain: Chain) -> Result<String, String> {
    let _guard = BusyGuard::acquire(chain)?;
    let reporter = Reporter::new(app, chain, "install");

    let (archive, format) = find_verified_archive(chain)
        .ok_or_else(|| "No verified snapshot download found. Download it first.".to_string())?;

    // Wait briefly for the daemon to release its files before touching anything.
    let deadline = Instant::now() + Duration::from_secs(30);
    while chain.daemon_running() {
        if Instant::now() > deadline {
            return Err(format!(
                "The {} node is still running. Stop it and try again.",
                chain.label()
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    let data_dir = chain.data_dir()?;
    let temp = data_dir.join(EXTRACT_DIR);
    if temp.exists() {
        fs::remove_dir_all(&temp)
            .map_err(|e| format!("Failed to clean old temp folder: {e}"))?;
    }
    fs::create_dir_all(&temp).map_err(|e| format!("Failed to create temp folder: {e}"))?;

    reporter.message("Preparing extraction...", 0.0);
    let extract_result = match format {
        ArchiveFormat::SevenZ => extract_7z(app, chain, &archive, &temp),
        ArchiveFormat::Zip => extract_zip(app, chain, &archive, &temp),
    };
    if let Err(e) = extract_result {
        let _ = fs::remove_dir_all(&temp);
        return Err(e);
    }

    let reporter = Reporter::new(app, chain, "install");
    reporter.message("Installing chain data...", 98.0);

    let root = match find_snapshot_root(&temp) {
        Some(root) => root,
        None => {
            let _ = fs::remove_dir_all(&temp);
            return Err(
                "Invalid snapshot: it must contain both 'blocks' and 'chainstate' folders."
                    .to_string(),
            );
        }
    };

    let installed = match swap_chain_dirs(&data_dir, &root) {
        Ok(list) => list,
        Err(e) => {
            let _ = fs::remove_dir_all(&temp);
            return Err(e);
        }
    };

    // Success - clean up the temp folder, the old chain data and the downloaded archive.
    let _ = fs::remove_dir_all(&temp);
    let _ = fs::remove_dir_all(data_dir.join(BACKUP_DIR));
    let _ = fs::remove_dir_all(data_dir.join(DOWNLOAD_DIR));

    reporter.message("Snapshot installed. Starting node...", 100.0);
    Ok(format!(
        "{} snapshot installed ({}).",
        chain.label(),
        installed.join(", ")
    ))
}

/// Returns the folder that directly contains `blocks/` and `chainstate/` - either the
/// extraction root or a single nested folder.
fn find_snapshot_root(base: &Path) -> Option<PathBuf> {
    let has_required = |dir: &Path| REQUIRED_DIRS.iter().all(|d| dir.join(d).is_dir());
    if has_required(base) {
        return Some(base.to_path_buf());
    }
    fs::read_dir(base)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir() && has_required(p))
}

/// Moves current chain folders aside, moves the snapshot folders in, and rolls
/// everything back if any step fails.
fn swap_chain_dirs(data_dir: &Path, snapshot_root: &Path) -> Result<Vec<String>, String> {
    let backup = data_dir.join(BACKUP_DIR);
    if backup.exists() {
        fs::remove_dir_all(&backup)
            .map_err(|e| format!("Failed to clear previous backup folder: {e}"))?;
    }
    fs::create_dir_all(&backup).map_err(|e| format!("Failed to create backup folder: {e}"))?;

    let mut moved_aside: Vec<&str> = Vec::new();
    let mut installed: Vec<&str> = Vec::new();

    let rollback = |moved_aside: &[&str], installed: &[&str]| {
        for d in installed {
            let _ = fs::remove_dir_all(data_dir.join(d));
        }
        for d in moved_aside {
            let _ = fs::rename(backup.join(d), data_dir.join(d));
        }
    };

    for d in CHAIN_DIRS {
        let current = data_dir.join(d);
        if current.exists() {
            if let Err(e) = fs::rename(&current, backup.join(d)) {
                rollback(&moved_aside, &installed);
                return Err(format!(
                    "Could not move existing '{d}' folder ({e}). Make sure the node is fully stopped and no other program is using the data folder."
                ));
            }
            moved_aside.push(d);
        }
    }

    for d in CHAIN_DIRS {
        let source = snapshot_root.join(d);
        if source.is_dir() {
            if let Err(e) = fs::rename(&source, data_dir.join(d)) {
                rollback(&moved_aside, &installed);
                return Err(format!(
                    "Failed to install '{d}' folder ({e}). Your previous chain data was restored."
                ));
            }
            installed.push(d);
        }
    }

    Ok(installed.iter().map(|s| s.to_string()).collect())
}

/// Rejects absolute paths and `..` so a malicious archive cannot write outside the
/// extraction folder.
fn sanitize_relative(name: &str) -> Option<PathBuf> {
    let normalized = name.replace('\\', "/");
    let mut out = PathBuf::new();
    for component in Path::new(&normalized).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Copies `reader` into `out`, reporting extraction progress and honouring cancel.
fn copy_with_progress(
    reader: &mut dyn Read,
    out: &Path,
    chain: Chain,
    reporter: &mut Reporter,
    done: &mut u64,
    total: u64,
) -> io::Result<()> {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut writer = BufWriter::with_capacity(IO_BUFFER, File::create(out)?);
    let mut buffer = vec![0u8; IO_BUFFER];
    loop {
        if cancelled(chain) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buffer[..n])?;
        *done += n as u64;
        reporter.bytes("Extracting", *done, total, false);
    }
    writer.flush()
}

fn extract_zip(app: &AppHandle, chain: Chain, archive: &Path, dest: &Path) -> Result<(), String> {
    let file = File::open(archive).map_err(|e| format!("Failed to open snapshot: {e}"))?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| format!("Invalid zip archive: {e}"))?;

    let total: u64 = (0..zip.len())
        .filter_map(|i| zip.by_index_raw(i).ok().map(|f| f.size()))
        .sum();

    let mut reporter = Reporter::new(app, chain, "extract");
    let mut done = 0u64;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| format!("Corrupt zip entry #{i}: {e}"))?;
        let Some(rel) = entry.enclosed_name().map(|p| p.to_path_buf()) else {
            continue;
        };
        let out = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| format!("Failed to create folder: {e}"))?;
            continue;
        }
        copy_with_progress(&mut entry, &out, chain, &mut reporter, &mut done, total)
            .map_err(|e| extraction_error(chain, e))?;
    }
    reporter.bytes("Extracting", done, total, true);
    Ok(())
}

fn extract_7z(app: &AppHandle, chain: Chain, archive: &Path, dest: &Path) -> Result<(), String> {
    let total: u64 = sevenz_rust::SevenZReader::open(archive, sevenz_rust::Password::empty())
        .map(|r| r.archive().files.iter().map(|f| f.size()).sum())
        .unwrap_or(0);

    let mut reporter = Reporter::new(app, chain, "extract");
    let mut done = 0u64;

    sevenz_rust::decompress_file_with_extract_fn(archive, dest, |entry, reader, _| {
        let Some(rel) = sanitize_relative(entry.name()) else {
            // Skip unsafe paths but still drain the stream for solid archives.
            io::copy(reader, &mut io::sink()).map_err(sevenz_rust::Error::io)?;
            return Ok(true);
        };
        let out = dest.join(rel);
        if entry.is_directory() {
            fs::create_dir_all(&out).map_err(sevenz_rust::Error::io)?;
        } else {
            copy_with_progress(reader, &out, chain, &mut reporter, &mut done, total)
                .map_err(sevenz_rust::Error::io)?;
        }
        Ok(true)
    })
    .map_err(|e| {
        if cancelled(chain) {
            "Install cancelled. Your existing chain data was not changed.".to_string()
        } else {
            format!("7z extraction failed (is the drive full?): {e}")
        }
    })?;

    reporter.bytes("Extracting", done, total, true);
    Ok(())
}

fn extraction_error(chain: Chain, e: io::Error) -> String {
    if cancelled(chain) || e.kind() == io::ErrorKind::Interrupted {
        "Install cancelled. Your existing chain data was not changed.".to_string()
    } else {
        format!("Extraction failed (is the drive full?): {e}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_relative("../evil").is_none());
        assert!(sanitize_relative("blocks/../../evil").is_none());
        assert!(sanitize_relative("/abs/path").is_none());
        assert_eq!(
            sanitize_relative("blocks/blk00000.dat"),
            Some(PathBuf::from("blocks").join("blk00000.dat"))
        );
        assert_eq!(
            sanitize_relative("snap\\chainstate\\CURRENT"),
            Some(PathBuf::from("snap").join("chainstate").join("CURRENT"))
        );
    }

    #[test]
    fn format_detection() {
        let mut e = SnapshotEntry {
            url: "https://x/y/hemp.7z".into(),
            sha256: None,
            size: None,
            extracted_size: None,
            height: None,
            date: None,
            format: None,
            notes: None,
        };
        assert!(ArchiveFormat::detect(&e).unwrap() == ArchiveFormat::SevenZ);
        e.url = "https://x/rvn.zip?token=1".into();
        assert!(ArchiveFormat::detect(&e).unwrap() == ArchiveFormat::Zip);
        e.url = "https://x/rvn.tar.gz".into();
        assert!(ArchiveFormat::detect(&e).is_err());
    }

    #[test]
    fn swap_and_root_detection() {
        let base = std::env::temp_dir().join(format!("h0x-snap-test-{}", uuid::Uuid::new_v4()));
        let data = base.join("data");
        let snap = base.join("extract").join("nested");
        for d in ["blocks", "chainstate", "assets"] {
            fs::create_dir_all(snap.join(d)).unwrap();
            fs::write(snap.join(d).join("new"), b"new").unwrap();
        }
        fs::create_dir_all(data.join("blocks")).unwrap();
        fs::write(data.join("blocks").join("old"), b"old").unwrap();
        fs::write(data.join("wallet.dat"), b"wallet").unwrap();

        let root = find_snapshot_root(&base.join("extract")).unwrap();
        assert_eq!(root, snap);
        let installed = swap_chain_dirs(&data, &root).unwrap();
        assert_eq!(installed, vec!["blocks", "chainstate", "assets"]);
        assert!(data.join("blocks").join("new").exists());
        assert!(data.join("wallet.dat").exists());
        assert!(data.join(BACKUP_DIR).join("blocks").join("old").exists());
        let _ = fs::remove_dir_all(&base);
    }
}
