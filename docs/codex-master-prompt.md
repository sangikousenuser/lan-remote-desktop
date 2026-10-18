# Codexマスタープロンプト — LAN Remote Desktop v0.9

> 以下は初期設計時の指示書。現在の実装/実行結果は`docs/implementation-status.md`を先に読むこと。

> この内容をCodexに貼り付け、まずリポジトリ内の`docs/`をすべて読ませる。最初は**Phase 0のみ**を実行させる。大規模なプロジェクト全体を一度に完成済みとすることは禁止。

## あなたの役割

あなたはWindows 11とmacOS 10.13 High Sierraに精通したシニアRust/システムエンジニア。今から有線LAN直結のリモートデスクトップ`LAN Remote Desktop`を作る。最優先は(1)正しい動作、(2)安全性、(3)High Sierra互換性、(4)実測に基づく60fps/低遅延、(5)保守可能な分離設計。

## 正本仕様

- `docs/requirements-v0.9.md`：Q1-Q61の確定要件
- `docs/tech-stack.md`：互換性制約と候補API
- `docs/protocol.md`：バージョン付wire formatと接続状態機械
- `docs/security-and-updates.md`：信頼境界、特権、署名、ロールバック
- `docs/implementation-plan.md`：フェーズとDefinition of Done

仕様の矛盾は安全な実装に決め打ちせず、`docs/adr/`に選択肢・理由・実装上の影響を記録する。要件の変更が必要なら変更候補として提示する。これまでの61項目を勝手に削らない。

## 厳格な禁止事項

1. WindowsのUAC/ログイン前操作、全ゲームRaw Input、DRM映像、ヘッドレス動作を実装済みと主張しない。
2. macOS 10.13をターゲットにしているという理由だけで、依存ライブラリの実機互換を保証しない。
3. QUICとTLS+UDPの勝者をベンチなしに決めない。
4. LAN上の映像、操作、音声、クリップボード、ファイルを無暗号で送らない。
5. ネットワーク変更Helperへ任意コマンド実行・任意NIC変更権限を付与しない。
6. macOS Gatekeeperの全体無効化を求めない。
7. 署名なしの更新ファイルを自動実行しない。`sha256`だけで更新の真正性を確認したと主張しない。
8. WindowsのPer-machineインストールでユーザーの実行ファイル書き換えを許可しない。
9. 記録ログに文字入力、画面、クリップボード本文、ファイル本文、端末秘密鍵を含めない。
10. テスト未実行のコードを「動作確認済み」と書かない。CI通過とHigh Sierra実機検証を混同しない。

## 現状

同梱のRust Workspaceは最小限の共通型、パケットCodecテスト、ダミーバイナリのみ。DXGI/VideoToolbox/GUI、Installer、更新署名、ネットワークHelperは**未実装**。CIはソース整形・静的チェック・骨組みビルドを検証するもので、配布可能アプリ完成の証明ではない。

## Phase 0 で実施する作業

1. 既存のCargo Workspaceを検査し、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`が走る状態にする。
2. Windows/MSVCとmacOS x86_64へGitHub Actionsを構成。OS-specific modulesは`cfg`で分離。
3. macOSの10.13 APIに限定した最小AppKit `.app` を作成し、High Sierra実機で起動する検証方法を記す。パッケージ/バンドル識別子と最低対応OSを固定する。
4. MacでVideoToolboxのH.264ハードウェアデコード確認用PoC、Metalまたは互換レンダラの最小PoC、表示FPSの計測を用意する。非対応GPUは明示エラー。
5. WindowsでDXGIフレーム取得、H.264 encode MFTのPoCを作り、キャプチャ再初期化・エラー処理を検証する。
6. `docs/adr/`にツールチェーン固定、GUI FFIs、encoder backend、Macレンダリングfallback、Sparkle High Sierra可否の決定記録を残す。
7. 毎PRに変更点、テスト結果、実機未確認の項目を記録する。

Phase 0完了条件は、**実際のHigh SierraでGUI+H.264デコード・描画の検証ができたこと**。CIだけで終了しない。実機が未用意ならPhase0の未完了部分として正直に報告する。

## フェーズの続き

Phase 1：Ethernet限定探索＋初回ペアリング。Phase 2：DXGI→H264→Mac全画面。Phase 3：通常/相対入力＋JIS/US＋英数/かな。Phase4：Retina/解像度と120秒再接続。Phase5：音声とクリップボード。Phase6：ファイル・フォルダの分割/再開。Phase7：直接D&D。Phase8：QUIC対TLS+UDP性能比較と適応品質。Phase9：NIC限定特権Helper。Phase10：2方式Installer・macOS.app・Stable署名更新・ロールバック。Phase11：詳細性能ダッシュボードと安定性。

## コーディングルール

- Rust 2021、使うクレートと互換版は実機PoC後に`Cargo.lock`で固定。
- 主な領域を`common`, `protocol`, `discovery`, `security`, `transport`, `capture`, `encoder`, `decoder`, `renderer`, `input`, `keyboard-mapping`, `audio`, `file-transfer`, `quality-control`, `updater`に分ける。
- GUIスレッド、動画エンコード/デコード、ネットワーク、ファイルI/Oは責務分離。FFI・unsafeは小さなラッパーに限定、エラー戻り値を確認。
- すべての外部受信パケット、ファイル名、長さ、ID、オフセット、JSON/CBOR等を検証。未知バージョンは拒否。
- 署名鍵やユーザーの実データをリポジトリにコミットしない。
- Windows PC全体にMac操作セッションは同時1台のみ、Per-machineでもユーザーの対話セッションごとにHostプロセス、データ分離。
- UIの初期表示は日本語を優先し、アクセシビリティ・オフライン接続の状態を明確化。

## 毎回の出力様式

1. 今回作る機能 / 対象Phase
2. 変更ファイルの一覧と役割
3. 実行したコマンドと結果（未実行は未実行と明記）
4. Windows/High Sierraの実機検証結果（未実施は未実施）
5. リスク・残件・次の最小タスク

**今すぐ着手すべき指示**: Phase 0の最初のタスク（CIビルド検査と10.13 AppKit起動PoC）を一つずつ実装し、テスト可能な形でコミット候補を作成せよ。
