// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
use std::fs;
use std::sync::Mutex;
use serde::{Deserialize, Serialize};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_updater::UpdaterExt;

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(Licensed(Mutex::new(false)))
        .invoke_handler(tauri::generate_handler![save_file, activate_license, check_license, app_version, check_update, install_update])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
