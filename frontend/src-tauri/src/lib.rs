use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use reqwest::header::RANGE;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::{fs::OpenOptions, io::AsyncWriteExt, sync::Mutex};

#[derive(Default)]
struct DownloadManager {
    tasks: Mutex<HashMap<String, DownloadTask>>,
    status: Arc<Mutex<HashMap<String, DownloadSnapshot>>>,
}

#[derive(Clone)]
struct DownloadTask {
    id: String,
    archive_url: String,
    config_url: String,
    token: String,
    dest_dir: PathBuf,
    app_dir: PathBuf,
    archive_path: PathBuf,
    config_path: PathBuf,
    paused: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    in_progress: Arc<AtomicBool>,
    status: Arc<Mutex<HashMap<String, DownloadSnapshot>>>,
}

#[derive(Serialize, Clone)]
struct DownloadEvent {
    id: String,
    downloaded: u64,
    total: Option<u64>,
    status: String,
    speed_bps: f64,
}

#[derive(Serialize, Clone)]
struct DownloadSnapshot {
    id: String,
    downloaded: u64,
    total: Option<u64>,
    status: String,
    speed_bps: f64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DownloadMeta {
    id: String,
    archive_url: String,
    config_url: String,
    dest_dir: String,
    token: Option<String>,
    total: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DownloadRequest {
    id: String,
    archive_url: String,
    config_url: String,
    dest_dir: String,
    token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResumeRequest {
    id: String,
    dest_dir: String,
    token: String,
}

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn get_default_apps_dir() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|_| "Failed to locate app binary.".to_string())?;
    let base = exe
        .parent()
        .ok_or_else(|| "Failed to resolve app directory.".to_string())?;
    let apps_dir = base.join("Apps");
    fs::create_dir_all(&apps_dir).map_err(|_| "Failed to create Apps folder.".to_string())?;
    Ok(apps_dir.to_string_lossy().to_string())
}

#[tauri::command]
async fn pick_install_dir() -> Result<Option<String>, String> {
    let handle = rfd::AsyncFileDialog::new()
        .set_title("Select Apps Folder")
        .pick_folder()
        .await;
    Ok(handle.map(|dir| dir.path().to_string_lossy().to_string()))
}

#[tauri::command]
async fn start_app_download(
    request: DownloadRequest,
    state: State<'_, DownloadManager>,
    app: AppHandle,
) -> Result<String, String> {
    if request.token.trim().is_empty() {
        return Err("Missing auth token.".to_string());
    }
    let mut tasks = state.tasks.lock().await;
    if let Some(existing) = tasks.get(&request.id) {
        if existing.in_progress.load(Ordering::SeqCst) {
            return Err("Download already in progress.".to_string());
        }
    }

    let dest_dir = PathBuf::from(&request.dest_dir);
    let app_dir = dest_dir.join(&request.id);
    let archive_path = app_dir.join(format!("{}.tar.gz", request.id));
    let config_path = app_dir.join(format!("{}.toml", request.id));

    let task = DownloadTask {
        id: request.id.clone(),
        archive_url: request.archive_url,
        config_url: request.config_url,
        token: request.token,
        dest_dir,
        app_dir,
        archive_path,
        config_path,
        paused: Arc::new(AtomicBool::new(false)),
        cancelled: Arc::new(AtomicBool::new(false)),
        in_progress: Arc::new(AtomicBool::new(true)),
        status: state.status.clone(),
    };

    tasks.insert(request.id.clone(), task.clone());
    drop(tasks);

    tauri::async_runtime::spawn(async move {
        let _ = write_download_meta(&task, None).await;
        update_status(
            &task.status,
            DownloadSnapshot {
                id: task.id.clone(),
                downloaded: 0,
                total: None,
                status: "downloading".to_string(),
                speed_bps: 0.0,
            },
        )
        .await;
        if let Err(err) = download_task(task.clone(), app.clone()).await {
            let _ = app.emit(
                "app_download_progress",
                DownloadEvent {
                    id: task.id.clone(),
                    downloaded: 0,
                    total: None,
                    status: format!("error:{err}"),
                    speed_bps: 0.0,
                },
            );
            update_status(
                &task.status,
                DownloadSnapshot {
                    id: task.id.clone(),
                    downloaded: 0,
                    total: None,
                    status: format!("error:{err}"),
                    speed_bps: 0.0,
                },
            )
            .await;
            task.in_progress.store(false, Ordering::SeqCst);
        }
    });

    Ok(request.id)
}

#[tauri::command]
async fn pause_download(id: String, state: State<'_, DownloadManager>) -> Result<(), String> {
    let tasks = state.tasks.lock().await;
    let task = tasks
        .get(&id)
        .ok_or_else(|| "Download not found.".to_string())?;
    task.paused.store(true, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
async fn resume_download(
    request: ResumeRequest,
    state: State<'_, DownloadManager>,
    app: AppHandle,
) -> Result<(), String> {
    let task = {
        let tasks = state.tasks.lock().await;
        tasks.get(&request.id).cloned()
    };

    let task = if let Some(task) = task {
        task
    } else {
        let app_dir = PathBuf::from(&request.dest_dir).join(&request.id);
        let meta_path = app_dir.join("download.json");
        if !meta_path.exists() {
            return Err("Download not found.".to_string());
        }
        let bytes = tokio::fs::read(&meta_path)
            .await
            .map_err(|_| "Failed to read download metadata.".to_string())?;
        let meta = serde_json::from_slice::<DownloadMeta>(&bytes)
            .map_err(|_| "Failed to parse download metadata.".to_string())?;
        let token = meta.token.unwrap_or_else(|| request.token.clone());
        if token.trim().is_empty() {
            return Err("Missing auth token.".to_string());
        }
        let archive_path = app_dir.join(format!("{}.tar.gz", meta.id));
        let config_path = app_dir.join(format!("{}.toml", meta.id));
        let task = DownloadTask {
            id: meta.id.clone(),
            archive_url: meta.archive_url,
            config_url: meta.config_url,
            token,
            dest_dir: PathBuf::from(meta.dest_dir),
            app_dir,
            archive_path,
            config_path,
            paused: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            in_progress: Arc::new(AtomicBool::new(false)),
            status: state.status.clone(),
        };
        let mut tasks = state.tasks.lock().await;
        tasks.insert(task.id.clone(), task.clone());
        task
    };

    if task.in_progress.load(Ordering::SeqCst) {
        return Err("Download already in progress.".to_string());
    }

    task.paused.store(false, Ordering::SeqCst);
    task.cancelled.store(false, Ordering::SeqCst);
    task.in_progress.store(true, Ordering::SeqCst);

    tauri::async_runtime::spawn(async move {
        if let Err(err) = download_task(task.clone(), app.clone()).await {
            let _ = app.emit(
                "app_download_progress",
                DownloadEvent {
                    id: task.id.clone(),
                    downloaded: 0,
                    total: None,
                    status: format!("error:{err}"),
                    speed_bps: 0.0,
                },
            );
            task.in_progress.store(false, Ordering::SeqCst);
        }
    });

    Ok(())
}

#[tauri::command]
async fn cancel_download(
    id: String,
    state: State<'_, DownloadManager>,
    app: AppHandle,
) -> Result<(), String> {
    let task = {
        let mut tasks = state.tasks.lock().await;
        tasks
            .remove(&id)
            .ok_or_else(|| "Download not found.".to_string())?
    };
    task.cancelled.store(true, Ordering::SeqCst);
    let _ = app.emit(
        "app_download_progress",
        DownloadEvent {
            id: id.clone(),
            downloaded: 0,
            total: None,
            status: "cancelled".to_string(),
            speed_bps: 0.0,
        },
    );
    let _ = fs::remove_file(&task.archive_path);
    let _ = fs::remove_dir_all(&task.app_dir);
    {
        let mut map = state.status.lock().await;
        map.remove(&id);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListDownloadsRequest {
    dest_dir: String,
}

#[tauri::command]
async fn list_downloads(
    request: ListDownloadsRequest,
    state: State<'_, DownloadManager>,
) -> Result<Vec<DownloadSnapshot>, String> {
    let mut results: HashMap<String, DownloadSnapshot> = HashMap::new();

    {
        let status = state.status.lock().await;
        for (id, snapshot) in status.iter() {
            results.insert(id.clone(), snapshot.clone());
        }
    }

    let base = PathBuf::from(request.dest_dir);
    if base.exists() {
        let mut entries = tokio::fs::read_dir(&base)
            .await
            .map_err(|_| "Failed to read install folder.".to_string())?;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let ft = entry
                .file_type()
                .await
                .map_err(|_| "Failed to read folder.".to_string())?;
            if !ft.is_dir() {
                continue;
            }
            let app_dir = entry.path();
            let meta_path = app_dir.join("download.json");
            if !meta_path.exists() {
                continue;
            }
            if let Ok(bytes) = tokio::fs::read(&meta_path).await {
                if let Ok(meta) = serde_json::from_slice::<DownloadMeta>(&bytes) {
                    if results.contains_key(&meta.id) {
                        continue;
                    }
                    let archive_path = app_dir.join(format!("{}.tar.gz", meta.id));
                    let downloaded = fs::metadata(&archive_path).map(|m| m.len()).unwrap_or(0);
                    results.insert(
                        meta.id.clone(),
                        DownloadSnapshot {
                            id: meta.id,
                            downloaded,
                            total: meta.total,
                            status: "paused".to_string(),
                            speed_bps: 0.0,
                        },
                    );
                }
            }
        }
    }

    Ok(results.values().cloned().collect())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveAppRequest {
    id: String,
    dest_dir: String,
}

#[tauri::command]
async fn remove_installed_app(request: RemoveAppRequest) -> Result<(), String> {
    let app_dir = PathBuf::from(request.dest_dir).join(&request.id);
    if app_dir.exists() {
        tokio::fs::remove_dir_all(&app_dir)
            .await
            .map_err(|_| "Failed to remove app.".to_string())?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListAppsRequest {
    dest_dir: String,
}

#[tauri::command]
async fn list_installed_apps(request: ListAppsRequest) -> Result<Vec<String>, String> {
    let path = PathBuf::from(request.dest_dir);
    if !path.exists() {
        return Ok(vec![]);
    }
    let mut results = Vec::new();
    let mut entries = tokio::fs::read_dir(&path)
        .await
        .map_err(|_| "Failed to read install folder.".to_string())?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Ok(ft) = entry.file_type().await {
            if ft.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    let content_dir = entry.path().join("content");
                    if content_dir.exists() {
                        results.push(name.to_string());
                    }
                }
            }
        }
    }
    Ok(results)
}

async fn download_task(task: DownloadTask, app: AppHandle) -> Result<(), String> {
    if task.token.trim().is_empty() {
        task.in_progress.store(false, Ordering::SeqCst);
        return Err("Missing auth token.".to_string());
    }
    if task.cancelled.load(Ordering::SeqCst) {
        task.in_progress.store(false, Ordering::SeqCst);
        return Ok(());
    }

    tokio::fs::create_dir_all(&task.app_dir)
        .await
        .map_err(|_| "Failed to create app directory.".to_string())?;

    let client = reqwest::Client::new();

    // Download config TOML (small)
    let cfg_resp = client
        .get(&task.config_url)
        .bearer_auth(&task.token)
        .send()
        .await
        .map_err(|_| "Failed to download config.".to_string())?;
    let cfg_bytes = cfg_resp
        .bytes()
        .await
        .map_err(|_| "Failed to read config.".to_string())?;
    tokio::fs::write(&task.config_path, cfg_bytes)
        .await
        .map_err(|_| "Failed to write config.".to_string())?;

    let mut downloaded = if task.archive_path.exists() {
        fs::metadata(&task.archive_path)
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };

    let mut request = client.get(&task.archive_url).bearer_auth(&task.token);
    if downloaded > 0 {
        request = request.header(RANGE, format!("bytes={}-", downloaded));
    }
    let response = request
        .send()
        .await
        .map_err(|_| "Failed to start download.".to_string())?;

    if response.status().as_u16() == 416 {
        // Already fully downloaded
        extracted_archive(&task.archive_path, &task.app_dir)?;
        task.in_progress.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "app_download_progress",
            DownloadEvent {
                id: task.id.clone(),
                downloaded,
                total: Some(downloaded),
                status: "completed".to_string(),
                speed_bps: 0.0,
            },
        );
        return Ok(());
    }

    if downloaded > 0 && response.status().as_u16() == 200 {
        // Server ignored range, restart download.
        downloaded = 0;
        tokio::fs::remove_file(&task.archive_path).await.ok();
    }

    let mut total = response
        .content_length()
        .map(|len| if downloaded > 0 { len + downloaded } else { len });
    let _ = write_download_meta(&task, total).await;

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&task.archive_path)
        .await
        .map_err(|_| "Failed to open download file.".to_string())?;

    let mut stream = response.bytes_stream();
    let mut last_emit = Instant::now();
    let mut last_emit_bytes = downloaded;

    while let Some(chunk) = stream.next().await {
        if task.cancelled.load(Ordering::SeqCst) {
            task.in_progress.store(false, Ordering::SeqCst);
            let _ = app.emit(
                "app_download_progress",
                DownloadEvent {
                    id: task.id.clone(),
                    downloaded,
                    total,
                    status: "cancelled".to_string(),
                    speed_bps: 0.0,
                },
            );
            update_status(
                &task.status,
                DownloadSnapshot {
                    id: task.id.clone(),
                    downloaded,
                    total,
                    status: "cancelled".to_string(),
                    speed_bps: 0.0,
                },
            )
            .await;
            return Ok(());
        }
        if task.paused.load(Ordering::SeqCst) {
            task.in_progress.store(false, Ordering::SeqCst);
            let _ = app.emit(
                "app_download_progress",
                DownloadEvent {
                    id: task.id.clone(),
                    downloaded,
                    total,
                    status: "paused".to_string(),
                    speed_bps: 0.0,
                },
            );
            update_status(
                &task.status,
                DownloadSnapshot {
                    id: task.id.clone(),
                    downloaded,
                    total,
                    status: "paused".to_string(),
                    speed_bps: 0.0,
                },
            )
            .await;
            return Ok(());
        }

        let chunk = chunk.map_err(|_| "Failed while downloading.".to_string())?;
        file.write_all(&chunk)
            .await
            .map_err(|_| "Failed to write download.".to_string())?;
        downloaded += chunk.len() as u64;

        if last_emit.elapsed() >= Duration::from_millis(250) {
            let elapsed = last_emit.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                (downloaded.saturating_sub(last_emit_bytes)) as f64 / elapsed
            } else {
                0.0
            };
            let _ = app.emit(
                "app_download_progress",
                DownloadEvent {
                    id: task.id.clone(),
                    downloaded,
                    total,
                    status: "downloading".to_string(),
                    speed_bps: speed,
                },
            );
            update_status(
                &task.status,
                DownloadSnapshot {
                    id: task.id.clone(),
                    downloaded,
                    total,
                    status: "downloading".to_string(),
                    speed_bps: speed,
                },
            )
            .await;
            last_emit = Instant::now();
            last_emit_bytes = downloaded;
        }
    }

    file.flush()
        .await
        .map_err(|_| "Failed to finalize download.".to_string())?;

    let _ = app.emit(
        "app_download_progress",
        DownloadEvent {
            id: task.id.clone(),
            downloaded,
            total,
            status: "installing".to_string(),
            speed_bps: 0.0,
        },
    );
    update_status(
        &task.status,
        DownloadSnapshot {
            id: task.id.clone(),
            downloaded,
            total,
            status: "installing".to_string(),
            speed_bps: 0.0,
        },
    )
    .await;

    extracted_archive(&task.archive_path, &task.app_dir)?;
    let _ = fs::remove_file(&task.archive_path);
    let _ = fs::remove_file(task.app_dir.join("download.json"));
    task.in_progress.store(false, Ordering::SeqCst);

    let _ = app.emit(
        "app_download_progress",
        DownloadEvent {
            id: task.id.clone(),
            downloaded,
            total,
            status: "completed".to_string(),
            speed_bps: 0.0,
        },
    );
    update_status(
        &task.status,
        DownloadSnapshot {
            id: task.id.clone(),
            downloaded,
            total,
            status: "completed".to_string(),
            speed_bps: 0.0,
        },
    )
    .await;

    Ok(())
}

fn extracted_archive(archive_path: &Path, app_dir: &Path) -> Result<(), String> {
    let content_dir = app_dir.join("content");
    fs::create_dir_all(&content_dir).map_err(|_| "Failed to create content directory.".to_string())?;
    let file = fs::File::open(archive_path).map_err(|_| "Failed to open archive.".to_string())?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(&content_dir)
        .map_err(|_| "Failed to extract archive.".to_string())?;
    Ok(())
}

async fn update_status(
    status: &Arc<Mutex<HashMap<String, DownloadSnapshot>>>,
    snapshot: DownloadSnapshot,
) {
    let mut map = status.lock().await;
    map.insert(snapshot.id.clone(), snapshot);
}

async fn write_download_meta(task: &DownloadTask, total: Option<u64>) -> Result<(), String> {
    let meta = DownloadMeta {
        id: task.id.clone(),
        archive_url: task.archive_url.clone(),
        config_url: task.config_url.clone(),
        dest_dir: task.dest_dir.to_string_lossy().to_string(),
        token: Some(task.token.clone()),
        total,
    };
    let path = task.app_dir.join("download.json");
    let data =
        serde_json::to_vec_pretty(&meta).map_err(|_| "Failed to encode download meta.".to_string())?;
    tokio::fs::write(path, data)
        .await
        .map_err(|_| "Failed to write download meta.".to_string())?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(DownloadManager::default())
        .invoke_handler(tauri::generate_handler![
            get_default_apps_dir,
            pick_install_dir,
            start_app_download,
            pause_download,
            resume_download,
            cancel_download,
            list_downloads,
            remove_installed_app,
            list_installed_apps
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
