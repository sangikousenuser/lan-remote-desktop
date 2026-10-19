# LAN Remote Desktop

Windows 11のログイン済みデスクトップを、Ethernet経由でMacから操作するネイティブアプリです。
バージョン **0.9.0-alpha.1**。接続機能を実装した開発版で、仕様全体の正式な完成判定には達していません。

## 起動

1. Windowsで `dist/LAN-Remote-Desktop-Windows-Host.zip` を展開し、PowerShellから `Start-Host.ps1` を実行します。表示された物理Ethernetの番号を入力します。
2. Macで `dist/<CPU>/LAN Remote Desktop.app` を開き、Ethernetを選択して「探索」→「接続」。初回は両方の端末で同じ確認コードを確認して許可します。
3. 2回目以降もMacで毎回接続を確認します。接続後は全画面になります。**Control+Option+Command+Esc**で入力をMacへ戻します。Control+Command+Fで全画面を解除します。

生成物:

- `dist/LAN-Remote-Desktop-Windows-Setup.exe`: NSISインストーラー。現在のユーザー／全ユーザーを選択可能。
- `dist/LAN-Remote-Desktop-Windows-Host.zip`: インストール不要のWindows版。
- `dist/x86_64-apple-darwin/LAN Remote Desktop.app`: Intel版、最低OS 10.13。High Sierra実機確認は未実施。
- `dist/aarch64-apple-darwin/LAN Remote Desktop.app`: Apple Silicon版、最低OS 11.0。この開発Macでの実行確認用。

Windowsの通知領域アイコンから、ファイル送信、受信先、転送画面、切断、接続許可、終了を操作できます。
接続中の性能ログは容量/期限を制限して各ユーザー領域へ保存します。両端の「性能ログを書き出す…」で内容を含まない数値JSONを保存できます。
Macは「表示」メニューからモニター／解像度、クリップボード、音声、相対マウス、Command変換を選べます。
ファイルは専用画面にドロップ可能です。受信先は両端の `Downloads/LAN Remote Desktop`。Explorerの任意フォルダーへ直接置くOLE転送は未実装です。
Macのアプリメニューでログイン時のEthernet監視を登録／解除できます。アプリは先にApplicationsへ移動してください。

接続できない場合やFirewallの限定設定は[実機試験・起動手順](docs/manual-testing.md)を参照してください。
IPv4設定のないEthernetは選択できません。Wi-Fiへ切り替えて接続する動作はありません。

## 現在の実装

TLS 1.3相互端末認証と両端承認、Ethernet拘束とUDP探索、DXGI→H.264→VideoToolbox/Metal、SendInput、かな/英数・JIS/US・Command変換、絶対／相対マウス、120秒復帰、PC全体の操作排他、モニター切替と表示モード復元、WASAPI→CoreAudio音声、テキスト/PNG同期、SHA-256付きファイル／フォルダー転送と永続再開、NSIS両インストールモード、LaunchAgentを実装しています。

未完了: QUICとTLS+UDPの本体組込み・実機比較と方式選定、権限付き固定IPウィザード、Explorer等への直接OLEドラッグ＆ドロップ、更新の管理者権限経路、正式署名公開、Windows新機能の実動作・High Sierra・長時間の受入試験。更新はHTTPS取得・独立署名・次回起動時helper・バックアップ/起動確認/復元まで実装していますが、正式Stableの配布はまだありません。管理者所有の配置は更新を保留します。[更新処理の範囲](docs/adr/0007-update-transactions.md)を参照してください。

[実装状況と検証結果](docs/implementation-status.md)で、実装、ローカル確認、実機未確認を区別しています。仕様の受入条件は維持しています。

## 開発

Rust 1.99.0とCargo.lockを使用します。

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo build --locked --workspace --release
python3 -m unittest discover -s packaging/macos -p 'test_*.py'
```

macOS / Command Line Tools:

```sh
python3 packaging/macos/package.py                             # Intel 10.13
python3 packaging/macos/package.py --target aarch64-apple-darwin # Apple Silicon
python3 scripts/smoke-macos.py 'dist/aarch64-apple-darwin/LAN Remote Desktop.app/Contents/MacOS/lrd-macos-client'
```

Mac SDK 27は旧OSにないSwift CoreMedia依存を付ける場合があるため、パッケージ処理は不適合を拒否します。この環境では26.5 SDKを選択しています。`--sdk /path/to/sdk` で明示できます。

Windows MSVC環境（配布時にRustは不要）:

```powershell
cargo build --locked --release -p lrd-windows-host -p lrd-update-helper
python3 packaging/windows/package.py --binary target/release/lrd-windows-host.exe --installer
```

NSIS 3が必要です。開発MacではMinGW版PEもクロスビルドしています。クロスビルドはWindows上の実行確認には代えられません。

## 仕様

- [統合要件 Q1–Q61](docs/requirements-v0.9.md)
- [実装工程](docs/implementation-plan.md)
- [実装状況](docs/implementation-status.md)
- [接続・実機試験](docs/manual-testing.md)
- [実装した通信のADR](docs/adr/0006-live-session.md)

公開リポジトリ: [sangikousenuser/lan-remote-desktop](https://github.com/sangikousenuser/lan-remote-desktop)。[GitHub Actions](https://github.com/sangikousenuser/lan-remote-desktop/actions)で3OSのビルド・テストを実行します。正式Releaseは未実装機能と実機ゲートの解消まで停止しています。

GitHubでログインして成功した **Rust CI** を開くと、ページ下部の **Artifacts** から `windows-host-alpha` と `intel-macos-client-alpha` を取得できます。Windows側はダウンロードした外側のActions ZIPを展開し、含まれるSetupまたはHost ZIPを使用してください。これらは署名付き自動更新用の正式Releaseではありません。
