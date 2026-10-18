# LAN Remote Desktop v0.9 — 実装工程・マイルストーン・受入基準

**開発環境**: GitHub Public。Windows + Intel macOSランナーでCI。High Sierra実機は手動テスト。性能評価は特定Mac/Windows GPUが確定するまで目標値扱い。

現在のコード・実測・未完了項目は[implementation-status.md](implementation-status.md)を参照。以下のExit criteriaは変更しない。

| Phase | 開発対象 | Exit criteria（必須） |
|---|---|---|
| 0 | Workspace、macOS 10.13 AppKit/GPU/VideoToolbox PoC、CI | macOS 10.13実機で最小GUIとH.264フレーム表示。機種/GPUと実際のFPSを記録 |
| 1 | Ethernet Link-Local、mDNS/UDP探索、端末ペアリング | DHCPなし直結で発見し、初回Windows/Mac双方確認。偽装端末拒否 |
| 2 | DXGI→H.264→VideoToolbox、AppKit全画面 | 選択モニターの実映像、実機対応時1080p/60fps、切断時入力遮断 |
| 3 | Absolute/relative mouse、JIS/US、英数/かな、Commandモード | 入力テスト、状態保持なし、ホットキー解除、IMEを切替可能 |
| 4 | 解像度交渉/Retina、選択モニター、120秒再接続 | Windows実モードのみ設定・変更前復元・瞬断時の表示維持 |
| 5 | WASAPI/Opus/Core Audio、同期クリップボード | 音が再生され、テキスト・PNG双方向ループなしで同期 |
| 6 | ファイル/フォルダ、hash、チャンク再開、専用UI | 1GB+転送を中断/再開してSHA-256一致、パス逸脱拒否 |
| 7 | OSネイティブ双方向Drag&Drop | Finder/Explorerテストで動作。未対応アプリは専用画面へfallback |
| 8 | QUIC vs TLS+UDP、Adaptive Quality | 同一環境で測定結果と採用決定ADRを作成。p50/p95/p99等を再現可能に記録 |
| 9 | 管理権限付きEthernet IP設定 | 初回権限/スコープ/失敗復元/リンクダウン復元、Wi-Fi維持 |
| 10 | Installer 2モード、Mac.app、Stable updater、署名Release | Per-user/Per-machine、High Sierra更新、署名検証・旧版rollback、環境承認後公開 |
| 11 | 詳細性能ダッシュボード、安定性検証 | 1時間安定テスト、ログ秘匿、切断復帰、メモリリーク不在を確認 |

**先行順序の例外**: 直接D&Dは専用転送が完成した後に実装。CIはPhase0から使い続ける。QUIC/TLS+UDPはPhase1から抽象化して開発し、最終採用だけPhase8で決定する。セキュリティ処理は後付けにせずPhase1から必須。

## 性能計測プロトコル

- 1080p、60fpsのテストパターン、Host GPU encode mode、bitrate、色形式を固定。
- RTTだけでなく画面の実表示フレーム間隔、入力→表示を高速度カメラまたはGPIO/LEDなど外部方法で可能な限り計測。録画ソフトだけの数値を入力表示遅延と呼ばない。
- 5分予備→10分測定、1時間ストレス。CPU/GPU、メモリ、帯域、frame age、drop、音声、切断復旧時間を記録。
- 60fpsは「対応機材で実画面が平均59fps以上、1% lowとドロップ率も測定」を暫定目標とする。画面更新がない静止時にはフレームを無理に送る必要なし。
- Windowsディスプレイ不対応モードは強制せず、解像度選択はMacの物理/論理サイズ＋GPU能力＋Windows対応モードから評価。
- 2台目Macの操作禁止、Hostのユーザーセッション交差拒否を必ず確認。

## 実装のPR分割

1. workspace skeleton + unit tests
2. network adapter selection / discovery
3. pairing + authenticated transport
4. Win DXGI capture dummy → actual adapter
5. Mac VideoToolbox rendering PoC
6. cursor/input/IME
7. session/display restoration
8. audio + clipboard
9. file engine + resumes
10. quality / compare transport
11. privileged network helper + restricted IPC
12. installers / secure auto-updaters
13. dashboards / release hardening

## タスクごとのDefinition of Done

- 変更は既存仕様書の要件IDを示す。
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`、対象OS向けbuildが成功。
- APIが不足している時は `TODO` と実装不能理由を表示し、架空の成功・見かけだけのUIを完成として扱わない。
- OS設定変更を伴う操作は復元テストを含む。
- リモート入力/転送は未認証時に拒否されることをテスト。
- 公式仕様変更やGPU非互換が発覚したらADR（Architecture Decision Record）を残し、勝手に要件を弱めない。
