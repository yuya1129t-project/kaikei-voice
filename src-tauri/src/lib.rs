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

// ───────── ストリーミング音声（whisper-server 常駐） ─────────
const WHISPER_PORT: u16 = 8765;

struct WhisperSrv(Mutex<Option<tauri_plugin_shell::process::CommandChild>>);

#[tauri::command]
async fn start_whisper(app: tauri::AppHandle, srv: tauri::State<'_, WhisperSrv>) -> Result<(), String> {
    if srv.0.lock().unwrap().is_some() { return Ok(()); }
    let model = app_dir(&app).join("ggml-small.bin");
    if !model.exists() { return Err("model_missing".into()); }
    let model_s = model.to_string_lossy().to_string();
    let (mut rx, child) = app
        .shell()
        .sidecar("whisper-server")
        .map_err(|e| e.to_string())?
        .args(["-m", &model_s, "--host", "127.0.0.1", "--port", &WHISPER_PORT.to_string(), "-t", "8", "-bs", "1", "-bo", "1", "-l", "ja"])
        .spawn()
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn(async move { while rx.recv().await.is_some() {} });
    *srv.0.lock().unwrap() = Some(child);
    Ok(())
}

#[tauri::command]
async fn stop_whisper(srv: tauri::State<'_, WhisperSrv>) -> Result<(), String> {
    let child = srv.0.lock().unwrap().take();
    if let Some(c) = child { let _ = c.kill(); }
    Ok(())
}

#[tauri::command]
async fn transcribe_chunk(
    state: tauri::State<'_, Licensed>,
    samples: Vec<f32>,
    prompt: String,
) -> Result<String, String> {
    if !*state.0.lock().unwrap() { return Err("ライセンスが有効ではありません".into()); }
    let mut buf = std::io::Cursor::new(Vec::<u8>::new());
    {
        let spec = hound::WavSpec { channels: 1, sample_rate: 16000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::new(&mut buf, spec).map_err(|e| e.to_string())?;
        for &s in &samples { w.write_sample((s.max(-1.0).min(1.0) * 32767.0) as i16).map_err(|e| e.to_string())?; }
        w.finalize().map_err(|e| e.to_string())?;
    }
    let wav = buf.into_inner();
    let part = reqwest::multipart::Part::bytes(wav).file_name("a.wav").mime_str("audio/wav").map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new()
        .text("language", "ja")
        .text("response_format", "text")
        .text("prompt", prompt)
        .part("file", part);
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/inference", WHISPER_PORT))
        .multipart(form)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    Ok(resp.text().await.map_err(|e| e.to_string())?.trim().to_string())
}

// ───────── 高精度オフライン認識（ReazonSpeech / sherpa-onnx-offline サイドカー） ─────────
fn reazon_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    app_dir(app).join("reazonspeech-ja")
}

#[tauri::command]
fn reazon_model_exists(app: tauri::AppHandle) -> bool {
    let d = reazon_dir(&app);
    d.join("tokens.txt").exists() && d.join("encoder.onnx").exists() && d.join("joiner.onnx").exists()
}

// ReazonSpeechモデルの必要4ファイルを直接DL（int8・計約169MB・解凍不要＝軽量＆高速）
#[tauri::command]
async fn download_reazon_model(app: tauri::AppHandle) -> Result<(), String> {
    use futures_util::StreamExt;
    use std::io::Write;
    let dir = reazon_dir(&app);
    if reazon_model_exists(app.clone()) { return Ok(()); }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    const BASE: &str = "https://github.com/yuya1129t-project/kaikei-voice/releases/download/model-rsp-ja-v1";
    // (ダウンロード元ファイル名, 保存名, 概算バイト) — totalはこの合計で進捗表示
    let files: [(&str, u64); 4] = [
        ("encoder.onnx", 154_670_139),
        ("decoder.onnx", 11_767_836),
        ("joiner.onnx",  2_696_970),
        ("tokens.txt",   45_754),
    ];
    let total: u64 = files.iter().map(|f| f.1).sum();
    let client = reqwest::Client::new();
    let mut downloaded: u64 = 0;
    let mut last: u64 = 0;
    for (name, _sz) in files.iter() {
        let url = format!("{}/{}", BASE, name);
        let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() { return Err(format!("DL失敗({}): {}", name, resp.status())); }
        let tmp = dir.join(format!("{}.part", name));
        {
            let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| e.to_string())?;
                file.write_all(&chunk).map_err(|e| e.to_string())?;
                downloaded += chunk.len() as u64;
                if downloaded - last >= 2_000_000 {
                    last = downloaded;
                    let _ = app.emit("model-progress", serde_json::json!({"downloaded": downloaded, "total": total}));
                }
            }
        }
        // 完走したファイルだけ正式名へ（途中で切れても reazon_model_exists が false のまま＝次回やり直し）
        std::fs::rename(&tmp, dir.join(name)).map_err(|e| e.to_string())?;
    }
    if !reazon_model_exists(app.clone()) { return Err("モデルDLに失敗（必要ファイルが不足）".into()); }
    let _ = app.emit("model-progress", serde_json::json!({"downloaded": total, "total": total, "done": true}));
    Ok(())
}

// 常駐WSサーバー方式（sherpa-onnx-offline-websocket-server をサイドカー起動＝モデル1回ロード）
const REAZON_PORT: u16 = 8766;
struct ReazonSrv {
    child: Mutex<Option<tauri_plugin_shell::process::CommandChild>>,
    log: std::sync::Arc<Mutex<Vec<String>>>,   // サイドカーの出力(stdout/stderr)を保持→失敗時の原因表示用
    port: Mutex<u16>,                          // 実際に使用中のポート（毎回動的に空きを取得）
}

// 前回起動時に残った（orphan）サイドカーを確実に終了。ポート使用中(EADDRINUSE)で
// sherpaが websocketpp 例外→即クラッシュするのを防ぐ＋多重常駐によるメモリ浪費も防ぐ。
fn kill_stale_sidecars() {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/IM", "sherpa-onnx-offline-websocket-server.exe"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW（黒い窓を出さない）
            .output();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = std::process::Command::new("pkill")
            .args(["-9", "-f", "sherpa-onnx-offline-websocket-server"])
            .output();
    }
    std::thread::sleep(std::time::Duration::from_millis(250)); // ポート解放を待つ
}

// 空きポートをOSに割り当ててもらう（衝突回避）
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
        .unwrap_or(REAZON_PORT)
}

fn reazon_push_log(buf: &std::sync::Arc<Mutex<Vec<String>>>, s: String) {
    if s.is_empty() { return; }
    let mut l = buf.lock().unwrap();
    l.push(s);
    let n = l.len();
    if n > 120 { l.drain(0..n - 120); }      // 直近120行だけ保持
}

// サーバー起動（未起動時のみ）。モデルをここで1回だけロード
fn reazon_start_inner(app: &tauri::AppHandle, srv: &ReazonSrv) -> Result<(), String> {
    let mut g = srv.child.lock().unwrap();
    if g.is_some() { return Ok(()); }
    let dir = reazon_dir(app);
    let (enc, dec, joi, tok) = (dir.join("encoder.onnx"), dir.join("decoder.onnx"), dir.join("joiner.onnx"), dir.join("tokens.txt"));
    if !tok.exists() || !enc.exists() { return Err("reazon_model_missing".into()); }
    kill_stale_sidecars();                 // 前回の残骸を掃除（ポート衝突→クラッシュ防止）
    let port = free_port();                // 毎回空きポートを取得して衝突を物理的に回避
    *srv.port.lock().unwrap() = port;
    srv.log.lock().unwrap().clear();
    let (mut rx, child) = app
        .shell()
        .sidecar("sherpa-onnx-offline-websocket-server")
        .map_err(|e| e.to_string())?
        .args([
            format!("--port={}", port),
            format!("--encoder={}", enc.to_string_lossy()),
            format!("--decoder={}", dec.to_string_lossy()),
            format!("--joiner={}", joi.to_string_lossy()),
            format!("--tokens={}", tok.to_string_lossy()),
            "--num-threads=2".to_string(),
        ])
        .spawn()
        .map_err(|e| e.to_string())?;
    let logbuf = srv.log.clone();
    tauri::async_runtime::spawn(async move {
        use tauri_plugin_shell::process::CommandEvent;
        while let Some(ev) = rx.recv().await {
            match ev {
                CommandEvent::Stdout(b) => reazon_push_log(&logbuf, String::from_utf8_lossy(&b).trim_end().to_string()),
                CommandEvent::Stderr(b) => reazon_push_log(&logbuf, String::from_utf8_lossy(&b).trim_end().to_string()),
                CommandEvent::Error(e) => reazon_push_log(&logbuf, format!("[error] {}", e)),
                CommandEvent::Terminated(t) => reazon_push_log(&logbuf, format!("[terminated] code={:?} signal={:?}", t.code, t.signal)),
                _ => {}
            }
        }
    });
    *g = Some(child);
    Ok(())
}

#[tauri::command]
fn reazon_load(app: tauri::AppHandle, srv: tauri::State<'_, ReazonSrv>) -> Result<(), String> {
    reazon_start_inner(&app, srv.inner())
}

#[tauri::command]
fn reazon_stop(srv: tauri::State<'_, ReazonSrv>) -> Result<(), String> {
    if let Some(c) = srv.child.lock().unwrap().take() { let _ = c.kill(); }
    Ok(())
}

// サイドカーの直近出力を返す（不具合診断用）
#[tauri::command]
fn reazon_log(srv: tauri::State<'_, ReazonSrv>) -> String {
    srv.log.lock().unwrap().join("\n")
}

// サーバー返却JSONから "text" を取り出す
// サーバー返却JSONから (text, 自信度=ys_log_probsの平均) を取り出す。
// 自信度が低い短文は雑音の幻聴の可能性が高いので、フロント側で破棄判定に使う。
fn reazon_extract_text(s: &str) -> (String, f32) {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s.trim()) {
        let text = v.get("text").and_then(|t| t.as_str()).map(|t| t.replace(' ', "")).unwrap_or_default();
        let conf = v.get("ys_log_probs").and_then(|a| a.as_array()).map(|a| {
            let vals: Vec<f64> = a.iter().filter_map(|x| x.as_f64()).collect();
            if vals.is_empty() { 0.0 } else { (vals.iter().sum::<f64>() / vals.len() as f64) as f32 }
        }).unwrap_or(0.0);
        return (text, conf);
    }
    (String::new(), 0.0)
}

// 録音PCM(16kHz mono f32)を 常駐サーバーへWSで送って文字化（高速）
#[tauri::command]
async fn transcribe_reazon(app: tauri::AppHandle, state: tauri::State<'_, Licensed>, srv: tauri::State<'_, ReazonSrv>, samples: Vec<f32>, sample_rate: Option<i32>) -> Result<String, String> {
    if !*state.0.lock().unwrap() { return Err("ライセンスが有効ではありません".into()); }
    reazon_start_inner(&app, srv.inner())?;                 // 未起動なら起動（モデル1回ロード）
    if samples.is_empty() { return Ok(String::new()); }
    // 実際の録音レートを使う（16kHz要求が無視される環境＝WindowsのWebView2でも正しく認識させる。sherpa側で16kHzへ再サンプリングされる）
    let sr = sample_rate.filter(|r| *r >= 8000 && *r <= 192000).unwrap_or(16000);
    // ペイロード: [i32 sample_rate][i32 byte長][f32 samples...]（リトルエンディアン）
    let byte_len = (samples.len() * 4) as i32;
    let mut payload = Vec::with_capacity(8 + samples.len() * 4);
    payload.extend_from_slice(&sr.to_le_bytes());
    payload.extend_from_slice(&byte_len.to_le_bytes());
    for &s in &samples { payload.extend_from_slice(&s.to_le_bytes()); }

    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let port = *srv.port.lock().unwrap();
    let url = format!("ws://127.0.0.1:{}", port);
    // サーバー起動直後はモデルロード中で接続不可なのでリトライ（最大~8秒）
    let mut ws = None;
    let mut last = String::new();
    for _ in 0..40 {
        match tokio_tungstenite::connect_async(url.as_str()).await {
            Ok((w, _)) => { ws = Some(w); break; }
            Err(e) => { last = e.to_string(); tokio::time::sleep(std::time::Duration::from_millis(200)).await; }
        }
    }
    let mut ws = match ws {
        Some(w) => w,
        None => {
            // 接続不可：サイドカーの出力を添えて返し、死んだプロセスは破棄して次回に再起動させる
            let log = srv.log.lock().unwrap().join(" / ");
            if let Some(c) = srv.child.lock().unwrap().take() { let _ = c.kill(); }
            let detail = if log.is_empty() { "(サイドカー出力なし＝起動に失敗の可能性)".to_string() } else { log };
            return Err(format!("音声サーバーに接続できません: {} ｜ {}", last, detail));
        }
    };
    ws.send(Message::Binary(payload)).await.map_err(|e| e.to_string())?;
    let mut result = String::new();
    let mut conf = 0f32;
    while let Some(m) = ws.next().await {
        match m {
            Ok(Message::Text(t)) => { let (tx, c) = reazon_extract_text(&t); result = tx; conf = c; break; }
            Ok(Message::Close(_)) => break,
            Ok(_) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    let _ = ws.close(None).await;
    // 文字＋自信度をJSONで返す（フロントが短文×低自信度を雑音として破棄できるように）
    Ok(serde_json::json!({"t": result, "c": conf}).to_string())
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
        .manage(WhisperSrv(Mutex::new(None)))
        .manage(ReazonSrv { child: Mutex::new(None), log: std::sync::Arc::new(Mutex::new(Vec::new())), port: Mutex::new(REAZON_PORT) })
        .invoke_handler(tauri::generate_handler![save_file, activate_license, check_license, app_version, check_update, install_update, model_exists, download_model, transcribe, start_whisper, stop_whisper, transcribe_chunk, reazon_model_exists, download_reazon_model, reazon_load, reazon_stop, reazon_log, transcribe_reazon])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
