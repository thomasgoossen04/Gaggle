use std::sync::Mutex as StdMutex;
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use futures_util::{StreamExt, TryStreamExt};
use reqwest::header::RANGE;
use reqwest::multipart::{Form, Part};
use reqwest::Body;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_opener::open_path;
use tokio::{fs::OpenOptions, io::AsyncWriteExt, sync::Mutex};
use tokio_util::io::ReaderStream;

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
async fn pick_upload_folder() -> Result<Option<String>, String> {
    let handle = rfd::AsyncFileDialog::new()
        .set_title("Select App Folder to Upload")
        .pick_folder()
        .await;
    Ok(handle.map(|dir| dir.path().to_string_lossy().to_string()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadAppRequest {
    server_ip: String,
    server_port: String,
    token: String,
    id: String,
    config_toml: String,
    folder_path: String,
}

#[derive(Serialize, Clone)]
struct UploadProgress {
    id: String,
    sent: u64,
    total: u64,
    pct: f64,
}

#[derive(Serialize, Clone)]
struct UploadStage {
    id: String,
    stage: String,
}

#[derive(Clone)]
struct UploadProgressState {
    sent: u64,
    total: u64,
    last_emit: Instant,
}

struct CountingReader<R: Read> {
    inner: R,
    state: Arc<StdMutex<UploadProgressState>>,
    app: AppHandle,
    id: String,
}

impl<R: Read> CountingReader<R> {
    fn new(
        inner: R,
        state: Arc<StdMutex<UploadProgressState>>,
        app: AppHandle,
        id: String,
    ) -> Self {
        Self {
            inner,
            state,
            app,
            id,
        }
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            let mut state = self.state.lock().unwrap();
            state.sent = state.sent.saturating_add(n as u64);
            let total = if state.total == 0 { 1 } else { state.total };
            if state.last_emit.elapsed() >= Duration::from_millis(150) {
                let pct = (state.sent as f64 / total as f64) * 100.0;
                let _ = self.app.emit(
                    "app_upload_progress",
                    UploadProgress {
                        id: self.id.clone(),
                        sent: state.sent,
                        total,
                        pct,
                    },
                );
                state.last_emit = Instant::now();
            }
        }
        Ok(n)
    }
}

fn add_dir_to_tar(
    builder: &mut tar::Builder<flate2::write::GzEncoder<std::fs::File>>,
    base: &Path,
    path: &Path,
    progress: &Arc<StdMutex<UploadProgressState>>,
    app: &AppHandle,
    id: &str,
) -> Result<(), String> {
    let entries = fs::read_dir(path).map_err(|_| "Failed to read folder.".to_string())?;
    for entry in entries {
        let entry = entry.map_err(|_| "Failed to read folder.".to_string())?;
        let entry_path = entry.path();
        let rel = entry_path
            .strip_prefix(base)
            .map_err(|_| "Failed to build archive path.".to_string())?;
        if entry_path.is_dir() {
            builder
                .append_dir(rel, &entry_path)
                .map_err(|_| "Failed to add folder to archive.".to_string())?;
            add_dir_to_tar(builder, base, &entry_path, progress, app, id)?;
        } else {
            let metadata = fs::metadata(&entry_path)
                .map_err(|_| "Failed to read file for archive.".to_string())?;
            let mut header = tar::Header::new_gnu();
            header.set_size(metadata.len());
            header.set_mode(0o644);
            header.set_mtime(
                metadata
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );
            header.set_cksum();

            let file = fs::File::open(&entry_path)
                .map_err(|_| "Failed to read file for archive.".to_string())?;
            let mut reader =
                CountingReader::new(file, progress.clone(), app.clone(), id.to_string());
            builder
                .append_data(&mut header, rel, &mut reader)
                .map_err(|_| "Failed to add file to archive.".to_string())?;
        }
    }
    Ok(())
}

fn dir_total_size(path: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    let entries = fs::read_dir(path).map_err(|_| "Failed to read folder.".to_string())?;
    for entry in entries {
        let entry = entry.map_err(|_| "Failed to read folder.".to_string())?;
        let entry_path = entry.path();
        if entry_path.is_dir() {
            total = total.saturating_add(dir_total_size(&entry_path)?);
        } else {
            total = total.saturating_add(
                fs::metadata(&entry_path)
                    .map_err(|_| "Failed to read file size.".to_string())?
                    .len(),
            );
        }
    }
    Ok(total)
}

#[tauri::command]
async fn upload_app(request: UploadAppRequest, app: AppHandle) -> Result<(), String> {
    if request.token.trim().is_empty() {
        return Err("Missing auth token.".to_string());
    }
    if request.id.trim().is_empty() {
        return Err("Missing app id.".to_string());
    }
    if request.config_toml.trim().is_empty() {
        return Err("Missing config.".to_string());
    }
    if request.folder_path.trim().is_empty() {
        return Err("Missing folder.".to_string());
    }

    let folder = PathBuf::from(&request.folder_path);
    if !folder.exists() {
        return Err("Folder not found.".to_string());
    }

    let temp_dir = std::env::temp_dir().join(format!("gaggle_upload_{}", request.id));
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir).ok();
    }
    fs::create_dir_all(&temp_dir).map_err(|_| "Failed to prepare temp folder.".to_string())?;
    let archive_path = temp_dir.join(format!("{}.tar.gz", request.id));

    let _ = app.emit(
        "app_upload_stage",
        UploadStage {
            id: request.id.clone(),
            stage: "compressing".to_string(),
        },
    );
    let total_size = dir_total_size(&folder)?;
    let progress_state = Arc::new(StdMutex::new(UploadProgressState {
        sent: 0,
        total: total_size,
        last_emit: Instant::now(),
    }));
    {
        let tar_file =
            fs::File::create(&archive_path).map_err(|_| "Failed to create archive.".to_string())?;
        let encoder = flate2::write::GzEncoder::new(tar_file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        add_dir_to_tar(
            &mut builder,
            &folder,
            &folder,
            &progress_state,
            &app,
            &request.id,
        )?;
        builder
            .finish()
            .map_err(|_| "Failed to finalize archive.".to_string())?;
    }
    let total = if total_size == 0 { 1 } else { total_size };
    let _ = app.emit(
        "app_upload_progress",
        UploadProgress {
            id: request.id.clone(),
            sent: total,
            total,
            pct: 100.0,
        },
    );

    let url = format!(
        "http://{}:{}/admin/apps/upload",
        request.server_ip.trim(),
        request.server_port.trim()
    );

    let archive_len = fs::metadata(&archive_path)
        .map_err(|_| "Failed to read archive size.".to_string())?
        .len();
    let file = tokio::fs::File::open(&archive_path)
        .await
        .map_err(|_| "Failed to open archive.".to_string())?;
    let mut sent: u64 = 0;
    let upload_id = request.id.clone();
    let app_handle = app.clone();
    let stream = ReaderStream::new(file).map_ok(move |chunk| {
        sent = sent.saturating_add(chunk.len() as u64);
        let total = if archive_len == 0 { 1 } else { archive_len };
        let pct = (sent as f64 / total as f64) * 100.0;
        let _ = app_handle.emit(
            "app_upload_progress",
            UploadProgress {
                id: upload_id.clone(),
                sent,
                total,
                pct,
            },
        );
        chunk
    });
    let body = Body::wrap_stream(stream);
    let archive_part = Part::stream_with_length(body, archive_len)
        .file_name(format!("{}.tar.gz", request.id))
        .mime_str("application/gzip")
        .map_err(|_| "Failed to set archive type.".to_string())?;

    let upload_id = request.id.clone();
    let form = Form::new()
        .text("id", upload_id.clone())
        .text("config", request.config_toml)
        .part("archive", archive_part);

    let client = reqwest::Client::new();
    let _ = app.emit(
        "app_upload_stage",
        UploadStage {
            id: upload_id.clone(),
            stage: "uploading".to_string(),
        },
    );
    let resp = client
        .post(url)
        .bearer_auth(request.token)
        .multipart(form)
        .send()
        .await
        .map_err(|_| "Upload failed.".to_string())?;

    fs::remove_dir_all(&temp_dir).ok();

    if !resp.status().is_success() {
        return Err(format!("Upload failed (HTTP {}).", resp.status()));
    }

    let _ = app.emit(
        "app_upload_progress",
        UploadProgress {
            id: upload_id,
            sent: archive_len,
            total: archive_len,
            pct: 100.0,
        },
    );

    Ok(())
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenAppFolderRequest {
    id: String,
    dest_dir: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunAppExecutableRequest {
    id: String,
    dest_dir: String,
    executable: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RunAppResult {
    duration_seconds: u64,
    exit_code: Option<i32>,
}

fn resolve_executable(app_dir: &Path, executable: &str) -> Result<PathBuf, String> {
    if executable.trim().is_empty() {
        return Err("Missing executable path.".to_string());
    }
    let rel = Path::new(executable);
    if rel.is_absolute() {
        return Err("Executable must be a relative path.".to_string());
    }
    let mut clean = PathBuf::new();
    for component in rel.components() {
        match component {
            std::path::Component::Normal(part) => clean.push(part),
            _ => return Err("Executable path is not safe.".to_string()),
        }
    }
    let full = app_dir.join("content").join(clean);
    if !full.exists() {
        return Err("Executable not found.".to_string());
    }
    Ok(full)
}

#[tauri::command]
fn open_app_folder(request: OpenAppFolderRequest) -> Result<(), String> {
    let app_dir = PathBuf::from(request.dest_dir).join(request.id);
    if !app_dir.exists() {
        return Err("App folder not found.".to_string());
    }
    open_path(app_dir, Option::<&str>::None).map_err(|_| "Failed to open app folder.".to_string())
}

#[tauri::command]
fn run_app_executable(request: RunAppExecutableRequest) -> Result<(), String> {
    let app_dir = PathBuf::from(request.dest_dir).join(&request.id);
    if !app_dir.exists() {
        return Err("App folder not found.".to_string());
    }
    let exec_path = resolve_executable(&app_dir, &request.executable)?;
    open_path(exec_path, Option::<&str>::None)
        .map_err(|_| "Failed to launch executable.".to_string())
}

#[tauri::command]
fn run_app_executable_tracked(request: RunAppExecutableRequest) -> Result<RunAppResult, String> {
    let app_dir = PathBuf::from(request.dest_dir).join(&request.id);
    if !app_dir.exists() {
        return Err("App folder not found.".to_string());
    }
    let exec_path = resolve_executable(&app_dir, &request.executable)?;
    let content_dir = app_dir.join("content");

    let start = Instant::now();
    let status = std::process::Command::new(&exec_path)
        .current_dir(&content_dir)
        .spawn()
        .and_then(|mut child| child.wait());

    let status = match status {
        Ok(status) => status,
        Err(_) => {
            // Fall back to default OS handler (e.g., README.txt).
            open_path(exec_path, Option::<&str>::None)
                .map_err(|_| "Failed to launch executable.".to_string())?;
            return Ok(RunAppResult {
                duration_seconds: 0,
                exit_code: None,
            });
        }
    };

    Ok(RunAppResult {
        duration_seconds: start.elapsed().as_secs(),
        exit_code: status.code(),
    })
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

    let total = response.content_length().map(|len| {
        if downloaded > 0 {
            len + downloaded
        } else {
            len
        }
    });
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
    fs::create_dir_all(&content_dir)
        .map_err(|_| "Failed to create content directory.".to_string())?;
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
    let data = serde_json::to_vec_pretty(&meta)
        .map_err(|_| "Failed to encode download meta.".to_string())?;
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
            pick_upload_folder,
            start_app_download,
            pause_download,
            resume_download,
            cancel_download,
            list_downloads,
            remove_installed_app,
            list_installed_apps,
            open_app_folder,
            run_app_executable,
            run_app_executable_tracked,
            upload_app
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
