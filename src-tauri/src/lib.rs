// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
use tauri_plugin_dialog::DialogExt;

// ネイティブの保存ダイアログを開き、選んだ場所にバイト列を書き出す。
// 保存したら true、キャンセルなら false を返す。
#[tauri::command]
fn save_file(app: tauri::AppHandle, default_name: String, contents: Vec<u8>) -> Result<bool, String> {
    let picked = app
        .dialog()
        .file()
        .set_file_name(&default_name)
        .blocking_save_file();
    match picked {
        Some(fp) => {
            let path = fp.into_path().map_err(|e| e.to_string())?;
            std::fs::write(&path, &contents).map_err(|e| e.to_string())?;
            Ok(true)
        }
        None => Ok(false),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![save_file])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
