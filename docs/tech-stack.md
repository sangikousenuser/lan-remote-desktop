# LAN Remote Desktop v0.9 — 技術スタック・互換性決定記録

更新: 2026-10-17。**仕様確定**と**実機検証後に採用**を明確に分ける。

## 技術マトリクス

| 部位 | 第一候補 | 代替/留意点 | 現状 |
|---|---|---|---|
| 共通言語 | Rust 2021 Edition, Cargo Workspace | 特定Rust版は検証後 `rust-toolchain.toml`で固定 | 骨組み用、可 |
| Async | `tokio` 1.x | High Sierraでネットワークとtimerを実機検証 | 候補 |
| macOS UI | `AppKit` + `objc2` 0.6系 / `objc2-app-kit` 0.3系 | Rustのみで難しい部分は Objective-C shim。Tauri 2は10.13必須の対象外 | 候補 |
| Windows API | `windows` (windows-rs) | Win32 / DXGI / D3D11 / COM / WASAPI | 候補 |
| Mac画面描画 | Metal / CVDisplayLink | Metal非対応GPU向けOpenGL系代替は別PoC、API availability確認 | 実機待ち |
| Windows映像取得 | `IDXGIOutputDuplication` | `DXGI_ERROR_ACCESS_LOST` / mode changeで再生成 | API選定 |
| Windows H.264 encode | Media Foundation MFT hardware encoder、D3D11 surface共有 | NVENC / AMF / QSVは個別バックエンドで拡張。実際のGPU次第 | 実機待ち |
| macOS H.264 decode | VideoToolbox `VTDecompressionSession` | pixel buffer zero-copy と 10.13互換性を検証 | 実機待ち |
| Audio | WASAPI Loopback → Opus → Core Audio | PCM PoC、Macのみ出力時のWindows側出音抑制は環境依存 | 候補 |
| QUIC比較 | `quinn` 0.11系 | UDP datagram・TLS 1.3・rustls暗号providerのmacOS互換確認 | 候補 |
| TLS+UDP比較 | `rustls` 0.23系 + TCP + 別UDP AEAD | UDP鍵導出・nonce・anti-replayを独自実装しないで実績あるライブラリで構成 | 候補 |
| シリアライズ | `serde` + `postcard` または `bincode` | セキュリティ境界では長さ・上限を明示 | 候補 |
| ログ | `tracing` + rolling file appender | ローカルのみ、内容のマスキング | 候補 |
| Mac自動起動 | LaunchAgent + SystemConfiguration | OSログイン時のユーザー権限内、初回設定が必要 | 採用方針 |
| Windows自動起動 | HKCU/HKLM Run + tray UI | 各Windowsログインユーザーの対話セッションに起動 | 採用方針 |
| Win installer | NSIS + MultiUser | Per-user/Per-machine を明確に分離 | 候補 |
| Macアップデータ | **Sparkle 2.9.6** | 10.13に対応する固定版、更新署名・次回起動反映を実機確認 | 候補 |
| Windowsアップデータ | Rust別プロセス updater | 署名検証・権限分離・トランザクション・ロールバック | 要実装 |
| SHA-256 | `sha2` 等 | チャンク・ファイル整合性検証。認証は署名と分離 | 候補 |
| Ed25519更新署名 | SparkleのEdDSA、Windowsは検証済みライブラリ | 公開鍵アプリ内固定、署名済マニフェスト、鍵ローテーション設計 | 要実装 |

**バージョン管理**: 上表は調査した候補レンジであり、Cargoの固定版を意味しない。Phase 0で `cargo tree`、MSRV、macOS 10.13の実機起動、セキュリティ更新状況を評価し、`Cargo.lock`と正確なツールチェーンを固定する。`obj-c` 系のABI、OpenSSL、`aws-lc-rs`のビルド動作にも注意する。

## macOS 10.13互換性ゲート

1. `MACOSX_DEPLOYMENT_TARGET=10.13` で `x86_64-apple-darwin` のReleaseビルド。
2. `otool -l` や `vtool` で最低動作OSと依存動的ライブラリを検証。
3. macOS 10.13実機に`.app`をコピーしてLaunchServices、AppKit、VideoToolbox、Metal/CVDisplayLink、Core Audioを起動。
4. 機種・GPU・画面解像度・有線リンク速度をログに記録（ユーザー同意のローカル診断のみ）。
5. 起動失敗・GPU非対応なら明示的な互換モードを表示し、60fps成功を偽装しない。
6. Sparkle 2.9.6の同梱、ED署名、次回起動時適用、失敗時の旧版保全を検証。Apple Developer IDがない場合、Gatekeeperの初回起動操作は別途案内。

## CI/CD ビルド互換性

- `windows-2025` と `macos-15-intel` を固定の初期CIランナー候補とする。将来ランナー廃止時に更新。
- OS依存APIモジュールは`cfg(target_os)`で分離し、共通プロトコルはLinuxでもテストできるようにする。
- 署名秘密鍵をPush/PR用Jobに渡さない。
- macOS 15 CI成功 ≠ macOS 10.13実機動作成功。リリース前にHigh Sierra実機手動チェックを必須とする。
- Source build向けCIと配布物の署名・公開処理は別。CI骨組みだけで実インストーラーや完全な.appの生成を主張しない。

## 最新の参照資料

- https://doc.rust-lang.org/nightly/rustc/platform-support/apple-darwin.html
- https://developer.apple.com/xcode/system-requirements
- https://docs.rs/objc2/latest/objc2/
- https://docs.rs/crate/objc2-app-kit/latest
- https://docs.rs/quinn/latest/quinn/struct.Connection.html
- https://docs.rs/rustls/latest/rustls/
- https://github.com/sparkle-project/Sparkle
