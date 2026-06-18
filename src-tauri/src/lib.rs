// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
use std::fs;
use std::sync::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{Manager, Emitter};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_updater::UpdaterExt;
use tauri_plugin_shell::ShellExt;

const LICENSE_SERVER: &str = "https://kaikei-license.yuya1129t.workers.dev";
const GRACE_DAYS: i64 = 14;

// ライセンス有効フラグ（中核機能のゲートに使用）
struct Licensed(Mutex<bool>);

#[derive(Serialize, Deserialize, Default)]
struct LicenseState {
    key: String,
    last_ok_at: Option<i64>, // unix秒
}

#[derive(Serialize)]
struct LicenseResult {
    ok: bool,
    error: Option<String>,
    offline: bool,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn app_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    let dir = app.path().app_data_dir().unwrap_or_else(|_| std::env::temp_dir());
    let _ = fs::create_dir_all(&dir);
    dir
}

// 端末固有のID（初回生成して保存）
fn device_id(app: &tauri::AppHandle) -> String {
    let p = app_dir(app).join("device_id.txt");
    if let Ok(s) = fs::read_to_string(&p) {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let _ = fs::write(&p, &id);
    id
}

fn load_state(app: &tauri::AppHandle) -> LicenseState {
    let p = app_dir(app).join("license.json");
    fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(app: &tauri::AppHandle, st: &LicenseState) {
    let p = app_dir(app).join("license.json");
    let _ = fs::write(&p, serde_json::to_string(st).unwrap_or_default());
}

// サーバーへ問い合わせ。(ok, error) を返す。通信失敗は Err。
async fn server_call(path: &str, key: &str, device: &str) -> Result<(bool, Option<String>), String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({ "key": key, "device_id": device, "device_name": "Windows PC" });
    let resp = client
        .post(format!("{}{}", LICENSE_SERVER, path))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
    let err = v.get("error").and_then(|x| x.as_str()).map(|s| s.to_string());
    Ok((ok, err))
}

// キー入力で有効化
#[tauri::command]
async fn activate_license(
    app: tauri::AppHandle,
    state: tauri::State<'_, Licensed>,
    key: String,
) -> Result<LicenseResult, String> {
    let dev = device_id(&app);
    match server_call("/activate", &key, &dev).await {
        Ok((true, _)) => {
            save_state(&app, &LicenseState { key, last_ok_at: Some(now_unix()) });
            *state.0.lock().unwrap() = true;
            Ok(LicenseResult { ok: true, error: None, offline: false })
        }
        Ok((false, err)) => Ok(LicenseResult { ok: false, error: err, offline: false }),
        Err(e) => Ok(LicenseResult { ok: false, error: Some(e), offline: true }),
    }
}

// 起動時の検証（オフラインは猶予期間内なら許可）
#[tauri::command]
async fn check_license(
    app: tauri::AppHandle,
    state: tauri::State<'_, Licensed>,
) -> Result<LicenseResult, String> {
    let st = load_state(&app);
    if st.key.is_empty() {
        *state.0.lock().unwrap() = false;
        return Ok(LicenseResult { ok: false, error: Some("no_license".into()), offline: false });
    }
    let dev = device_id(&app);
    match server_call("/validate", &st.key, &dev).await {
        Ok((true, _)) => {
            save_state(&app, &LicenseState { key: st.key, last_ok_at: Some(now_unix()) });
            *state.0.lock().unwrap() = true;
            Ok(LicenseResult { ok: true, error: None, offline: false })
        }
        Ok((false, err)) => {
            *state.0.lock().unwrap() = false;
            Ok(LicenseResult { ok: false, error: err, offline: false })
        }
        Err(_) => {
            let within = st
                .last_ok_at
                .map(|t| now_unix() - t < GRACE_DAYS * 86400)
                .unwrap_or(false);
            *state.0.lock().unwrap() = within;
            Ok(LicenseResult {
                ok: within,
                error: if within { None } else { Some("offline_no_grace".into()) },
                offline: true,
            })
        }
    }
}

// ネイティブの保存ダイアログ＋書き出し（ライセンス無効時は拒否）
// 非同期コマンド＋ノンブロッキングのダイアログにすることで、macOSのメインスレッド
// ブロック（固まり→強制終了）を回避する。
#[tauri::command]
async fn save_file(
    app: tauri::AppHandle,
    state: tauri::State<'_, Licensed>,
    default_name: String,
    contents: Vec<u8>,
) -> Result<bool, String> {
    let licensed = *state.0.lock().unwrap();
    if !licensed {
        return Err("ライセンスが有効ではありません".into());
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(&default_name)
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    match rx.await.map_err(|e| e.to_string())? {
        Some(fp) => {
            let path = fp.into_path().map_err(|e| e.to_string())?;
            std::fs::write(&path, &contents).map_err(|e| e.to_string())?;
            Ok(true)
        }
        None => Ok(false),
    }
}

// アプリのバージョン（画面表示・更新確認用）
#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// 更新確認：新しいバージョンがあればその版番号を返す（無ければ None）
#[tauri::command]
async fn check_update(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| e.to_string())?;
    Ok(update.map(|u| u.version))
}

// 更新の適用：ダウンロード＆インストールして再起動
#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<bool, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    if let Some(update) = updater.check().await.map_err(|e| e.to_string())? {
        update
            .download_and_install(|_chunk, _total| {}, || {})
            .await
            .map_err(|e| e.to_string())?;
        app.restart();
    }
    Ok(false)
}

// ───────── 音声入力（ローカルWhisper） ─────────

// 音声モデルが端末にあるか
#[tauri::command]
fn model_exists(app: tauri::AppHandle) -> bool {
    app_dir(&app).join("ggml-small.bin").exists()
}

// 音声モデル（small・約466MB）を初回ダウンロード（ストリーミング保存）
#[tauri::command]
async fn download_model(app: tauri::AppHandle) -> Result<(), String> {
    use futures_util::StreamExt;
    use std::io::Write;
    let dir = app_dir(&app);
    let path = dir.join("ggml-small.bin");
    if path.exists() {
        return Ok(());
    }
    let tmp = dir.join("ggml-small.bin.part");
    let url = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin";
    let resp = reqwest::get(url).await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("ダウンロード失敗: {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;
    let mut last: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        if downloaded - last >= 2_000_000 {
            last = downloaded;
            let _ = app.emit("model-progress", serde_json::json!({"downloaded": downloaded, "total": total}));
        }
    }
    drop(file);
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    let _ = app.emit("model-progress", serde_json::json!({"downloaded": total, "total": total, "done": true}));
    Ok(())
}

// 録音PCM(16kHz mono f32)を受け取り、whisper-cliで文字化して返す
#[tauri::command]
async fn transcribe(
    app: tauri::AppHandle,
    state: tauri::State<'_, Licensed>,
    samples: Vec<f32>,
    prompt: String,
) -> Result<String, String> {
    let licensed = *state.0.lock().unwrap();
    if !licensed {
        return Err("ライセンスが有効ではありません".into());
    }
    let dir = app_dir(&app);
    let model = dir.join("ggml-small.bin");
    if !model.exists() {
        return Err("model_missing".into());
    }
    let wav = dir.join("rec.wav");
    {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&wav, spec).map_err(|e| e.to_string())?;
        for &s in &samples {
            let v = (s.max(-1.0).min(1.0) * 32767.0) as i16;
            w.write_sample(v).map_err(|e| e.to_string())?;
        }
        w.finalize().map_err(|e| e.to_string())?;
    }
    let model_s = model.to_string_lossy().to_string();
    let wav_s = wav.to_string_lossy().to_string();
    let output = app
        .shell()
        .sidecar("whisper-cli")
        .map_err(|e| e.to_string())?
        .args([
            "-m", &model_s, "-f", &wav_s, "-l", "ja",
            "--prompt", &prompt, "-np", "-nt", "-t", "4",
        ])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(text)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_shell::init())
        .manage(Licensed(Mutex::new(false)))
        .invoke_handler(tauri::generate_handler![save_file, activate_license, check_license, app_version, check_update, install_update, model_exists, download_model, transcribe])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
