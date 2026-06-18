# KaikeiVoice（音声入力 → 弥生CSV）デスクトップ版

会計士向け音声入力ツールのデスクトップアプリ（Tauri v2）。
フロントエンド（`src/index.html`）に従来の音声入力アプリを丸ごと組み込み、
オフライン動作（encoding-japaneseは `src/vendor/` に同梱）。

## 開発（Mac / 動作確認）

```bash
cd desktop
npm install                 # 初回のみ
npm run tauri dev           # デスクトップウィンドウで起動（初回はRustビルドで数分）
```

ウィンドウが開き、入力・解析・表・CSV出力が「アプリ」として動きます。
※ 音声ボタン（Web Speech API）はデスクトップでは動きません。段階2で同梱Whisperに置き換え予定。
　 それ以外（テキスト入力・解析・修正・弥生CSV/Excel出力）は動作します。

## Mac上でアプリを作る（手元確認用）

```bash
npm run tauri build         # dist: src-tauri/target/release/bundle/ に .app / .dmg
```

## Windows版(.exe)を作る（本番配布・Macからでも可）

Windows実機は不要。GitHub Actions（windows-latest）でビルドします。

1. このフォルダをGitHubリポジトリに push
2. GitHubの **Actions** タブ → **Build Windows** → **Run workflow**
3. 完了後、成果物 **KaikeiVoice-windows**（`*-setup.exe` / `*.msi`）をダウンロード

タグ（例 `v1.0.0`）を push しても自動ビルドされます。

## 構成

- `src/index.html` … アプリ本体（UI＋パーサ＋弥生CSV生成。自己完結）
- `src/vendor/encoding.min.js` … Shift_JIS変換（オフライン用に同梱）
- `src-tauri/` … Tauri（Rust）側。`tauri.conf.json` でウィンドウ/アプリ名を設定

## 今後の段階

- 段階2：ローカル音声エンジン（Whisper）を同梱し、録音→アプリ内で文字化（オフライン・ブラウザ非依存）
- 段階3：ライセンス認証・課金・コード署名・自動更新
