use std::fs::{self, File};
use std::io::{self, Read, Write};
use tauri::{AppHandle, Emitter};
use crate::modules::rvn_rpc::rvn_data_dir;

#[derive(Clone, serde::Serialize)]
struct ProgressPayload {
    status: String,
    progress: f64,
}

#[tauri::command]
pub async fn rvn_download_snapshot(app_handle: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let data_dir = rvn_data_dir()?;
        let snapshot_url = "http://127.0.0.1:8000/bootstrap.zip"; // TODO: Update with real URL once uploaded
        let dest_path = data_dir.join("bootstrap.zip");

        // 1. Emit start event
        let _ = app_handle.emit("snapshot-progress", ProgressPayload {
            status: format!("Connecting to snapshot server..."),
            progress: 5.0,
        });

        // 2. Download the file
        let mut response = reqwest::blocking::get(snapshot_url)
            .map_err(|e| format!("Failed to connect to snapshot server: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Server returned error: {}", response.status()));
        }

        let total_size = response.content_length().unwrap_or(0);
        let mut file = File::create(&dest_path)
            .map_err(|e| format!("Failed to create local file: {}", e))?;
        
        let mut downloaded: u64 = 0;
        let mut buffer = [0; 8192];

        let _ = app_handle.emit("snapshot-progress", ProgressPayload {
            status: "Downloading snapshot archive...".to_string(),
            progress: 10.0,
        });

        let mut last_emitted_mb = 0;
        while let Ok(usize) = response.read(&mut buffer) {
            if usize == 0 { break; }
            downloaded += usize as u64;
            let _ = file.write_all(&buffer[..usize]);
            
            let current_mb = downloaded / 1_000_000;
            if total_size > 0 && current_mb > last_emitted_mb + 50 { // Emit every ~50 MB
                last_emitted_mb = current_mb;
                let p = 10.0 + (downloaded as f64 / total_size as f64) * 50.0;
                let _ = app_handle.emit("snapshot-progress", ProgressPayload {
                    status: format!("Downloading... {} MB", current_mb),
                    progress: p,
                });
            }
        }
        
        // Force sync the file to disk and drop the handle before extracting
        let _ = file.sync_all();
        drop(file);

        // 3. Extract the zip file
        let _ = app_handle.emit("snapshot-progress", ProgressPayload {
            status: "Download complete. Cleaning old database...".to_string(),
            progress: 60.0,
        });

        // Clean old blocks and chainstate to prevent corruption
        let _ = fs::remove_dir_all(data_dir.join("blocks"));
        let _ = fs::remove_dir_all(data_dir.join("chainstate"));

        let _ = app_handle.emit("snapshot-progress", ProgressPayload {
            status: "Extracting new database (this may take a few minutes)...".to_string(),
            progress: 65.0,
        });

        let file = File::open(&dest_path).map_err(|e| format!("Failed to open zip: {}", e))?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Invalid zip archive: {}", e))?;
        let archive_len = archive.len();
        
        for i in 0..archive_len {
            let mut file = archive.by_index(i).unwrap();
            let outpath = match file.enclosed_name() {
                Some(path) => data_dir.join(path),
                None => continue,
            };

            if (*file.name()).ends_with('/') {
                fs::create_dir_all(&outpath).unwrap();
            } else {
                if let Some(p) = outpath.parent() {
                    if !p.exists() {
                        fs::create_dir_all(&p).unwrap();
                    }
                }
                let mut outfile = File::create(&outpath).unwrap();
                io::copy(&mut file, &mut outfile).unwrap();
            }

            // Update extraction progress (throttle to every 5 files)
            let p = 60.0 + (i as f64 / archive_len as f64) * 40.0;
            if i % 5 == 0 || i == archive_len - 1 {
                let _ = app_handle.emit("snapshot-progress", ProgressPayload {
                    status: format!("Extracting files... {}/{}", i + 1, archive_len),
                    progress: p,
                });
            }
        }

        let _ = app_handle.emit("snapshot-progress", ProgressPayload {
            status: "Extraction Complete. Booting node...".to_string(),
            progress: 100.0,
        });

        // Cleanup
        let _ = fs::remove_file(dest_path);

        Ok("Snapshot downloaded and extracted successfully.".to_string())
    })
    .await
    .map_err(|e| format!("Snapshot task failed: {}", e))?
}
