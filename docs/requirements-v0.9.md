# LAN Remote Desktop — 統合要件定義・技術設計書 v0.9

- 更新日: 2026-10-17
- ステータス: Q1–Q61確定・要件定義完了（実装と性能検証は未実施）
- 開発言語: Rust (OS固有APIにはObjective-C / C / Win32 FFIを許容)
- Host: Windows 11 Pro (ユーザーがログイン済みのデスクトップ)
- Client: macOS High Sierra 10.13 / Intel x86_64
- ネットワーク: WindowsとMacをEthernetで直結。リモートデスクトップは有線に固定。Wi-Fiはインターネット用に維持
- 運用コスト: LAN利用・基本機能に有料サーバー不要。GitHub公開リポジトリで標準CIを実行

## 1. 目的とシステム境界

有線LANで接続すると、Macの常駐AgentがWindows Hostを発見してMacアプリを自動起動し、ユーザーの接続承認後にWindows画面を全画面表示する。可能な端末で1080p/60fpsと入力から表示まで50ms以下を開発目標とする。高画質・低遅延、システム音声、キー/マウス、双方向クリップボード、ファイル/フォルダ転送を提供する。

RDPのような別セッションではなく、Windowsログイン済みコンソール画面をキャプチャする。Windowsログイン前、UACセキュアデスクトップ、あらゆるゲームのRaw Input、DRM映像、ヘッドレスでの確実な動作は初期正式対応範囲外。高画質・60fpsの達成は実際のGPU/ディスプレイ/LANに依存する。

## 2. 主要アーキテクチャ

```text
                 （更新処理のみインターネットが必要）
Windows Host  ------- GitHub Releases ------- macOS Client
  |                                                |
  | DXGI → H.264 HW Encoder                        | LaunchAgent → Host発見
  | WASAPI loopback                                | VideoToolbox → GPU描画
  | SendInput / 入力受信                            | AppKit GUI / CoreAudio
  | Clipboard / Transfer / Updater                 | Clipboard / Transfer / Updater
  |                                                |
  +====== Ethernet直結: 暗号化映像/音声/入力/ファイル ======+
                      （Wi-Fiへのフォールバックなし）
```

Rust Cargo Workspace: `common`, `protocol`, `discovery`, `security`, `transport`, `capture`, `encode`, `decode`, `renderer`, `audio`, `input`, `keyboard-mapping`, `display-manager`, `clipboard`, `file-transfer`, `quality-control`, `agent`, `updater`, `installer` を分離する。最初から通信プロトコルのバージョン管理を導入する。

## 3. 発見・認証・セッション管理

- macOSログイン時にLaunchAgentでネットワーク監視を開始し、Ethernetリンクアップ後にHostを探索する。mDNS/DNS-SDを第一候補に、対象インターフェース内のUDP探索を代替案とする。
- IP自動設定(DHCPおよびIPv4 Link-Local)後にHost探索。インターネットやルーターを要求しない。
- 初回のみWindowsとMac双方で端末鍵を確認してペアリングし、端末鍵の識別と暗号化通信を確立する。
- 2回目以降、Macで毎回接続確認を出す。拒否されたHostへの通知連打は禁止。
- ユーザー承認後、能力交渉・表示モード選定・映像/音声/入力セッション開始を行う。
- 手動切断は自動再接続対象外。セッション入力イベントを即座に無効化する。
- 接続状態: `idle`, `discovered`, `awaiting_approval`, `connecting`, `connected`, `reconnecting`, `closed`, `error`。

## 4. Ethernetインターフェース固定・固定IP

- 全リモート通信(探索、認証、映像、音声、入力、クリップボード、ファイル)を明示したEthernetインターフェースへ拘束する。送信元と経路を検証し、切断時にWi-Fiへ回避しない。
- MacのWi-Fiデフォルトルート/DNSを維持する。固定IP設定で直結Ethernetにデフォルトゲートウェイを作らない。
- アドレス取得失敗時のみ、ユーザー承認とOS管理者権限を要求して、直結Ethernetにプライベート固定IPを付与可能にする。例: Windows `10.254.240.1/30`, Mac `10.254.240.2/30`（衝突検査後に限る）。
- **IP通信不能な状態では、片側アプリだけで相手側のIP設定を変更できるとは限らない。** 各端末のローカル設定ウィザード/事前設定で協調させる。
- 適用前に各OSのインターフェース設定をスナップショット保存。リンクダウン時は権限を持つ設定ヘルパーが復元する。切断イベントを取り逃した場合に備え次回起動時照合・復元、失敗時のタイムアウトロールバックを設ける。
- ユーザーが設定を手動変更した場合は無条件上書きをせず差分確認する。Windows Firewallは必要最小限の規則だけ追加する。

## 5. 映像キャプチャ・転送・描画

- HostでDXGI Desktop Duplicationを使用してディスプレイ一覧を取得し、選択した1画面をキャプチャする。モニター切替は上部ツールバーとショートカット。
- H.264を必須。Windowsのハードウェアエンコードを優先し、対応GPUでHEVCをオプションにする。High Sierra上でVideoToolboxの対応能力を事前検証する。
- ClientはVideoToolboxでデコードしMetal優先で描画。High Sierra GPUのAPI対応を確認し代替描画を設ける。
- 目標は1080p/60fps、対応環境で1440p/4K。60fpsは16.67msのフレーム間隔であり、総入力遅延の保証値ではない。静止画は不要な同一フレームを省略可能。
- Adaptive Quality: 画面の変化量・入力頻度・フレームドロップ・キャプチャ/Encode/Decode/Render時間・RTT・パケット損失を元に、文字中心は鮮明さ、ゲーム/動画は60fps/低遅延を重視する。ヒステリシスによって頻繁な切替を防止する。
- Retina: Macの物理/論理解像度、スケール、ディスプレイリフレッシュレート、デコード性能を取得。Windowsが対応する表示モードのみを選び、Mac側で比率を維持して拡大縮小する。
- 解像度変更前にWindows設定を保存し、変更失敗/セッション終了時に復元。接続中にユーザーが変更したモードは勝手に上書きしない。

## 6. 文字入力・ゲーム入力・UI

- MacのJIS/USキーボードを自動検出。物理キー/論理キー/テキスト入力を明確に区別する。
- Macの「かな」はWindowsのIME ON、「英数」はIME OFF（Microsoft IMEを主要検証対象）。必要に応じWindows IMEモードを確認し、トグルによる状態不一致を避ける。
- CommandキーマッピングはMac風(Command+C→Ctrl+Cなど)・Windows風(Command→Windowsキー)を切替可能とする。
- 通常マウス絶対座標入力、ドラッグ/ホイールと、ゲーム用相対移動を提供する。Windows側のRaw Input専用ゲームまで保証しない。
- Hostの実行アプリ/映像挙動等から相対マウスモード切替を**提案**するが、ユーザーの確認なく勝手にマウスを拘束しない。専用ショートカットからも切替できる。
- macOS AppKitを使い全画面表示。上部にマウス移動時だけ操作ツールバーを表示し、ショートカットでもMac側のマウス/キーボードを取り戻せる。

## 7. 音声、クリップボード、ファイル

### 音声
- Windows WASAPI Loopback → (Opus第一候補) → macOS Core Audio。
- 出力モードはWindowsのみ、Macのみ、両方。Windowsのマイク転送は不要。
- Windows音声をMacだけから鳴らす際は追加仮想ドライバーなしを優先。取得音声を無効化する単純なWindowsミュートは行わず、機器制約があれば機能制限を案内する。

### クリップボード
- Windows↔Macでテキストと画像を自動同期。重複検出、更新ID、上限サイズ、形式検証、同期停止ボタンを設ける。接続中のみに限定。

### ファイル
- 双方向のファイル・フォルダ、サブフォルダ階層、複数選択、進捗、キャンセル、SHA-256等による完全性検証。
- 既定の受信先: macOS `~/Downloads/LAN Remote Desktop/`、Windows `%USERPROFILE%\Downloads\LAN Remote Desktop\`。
- ペアリング済みの信頼済み端末からは設定されたフォルダに自動保存可能。未知の端末に対してはまずペアリングを要求し、無断転送は不可。
- チャンク、Transfer ID、マニフェスト、検証済みデータ位置をローカルへ保持。再接続時は同じ端末の未完了転送を自動照合し、元ファイルの変更があればユーザーへ再確認。一時ファイル→検証→アトミックに完成配置。
- 開発順序: **最初に専用ファイル転送画面を完成させ、その後、Mac↔Windowsの直接ドラッグ＆ドロップに対応**。Windows Explorer等のOS/アプリ固有のOLE転送互換は別途検証し、未対応アプリは専用画面へフォールバック。

## 8. セッションの120秒自動復帰

- LAN切断時は入力を直ちに無効化し、`reconnecting` に遷移。認証済みHostの同一端末鍵にのみ復帰可能。
- 切断後120秒以内、手動切断されておらず鍵とペアリングが変わっていないときだけ、Macの再承認なしで再接続。新セッション鍵と新しい入力世代IDを付与し、古い入力イベントは破棄。
- 接続復帰後は映像/音声/クリップボードを再構築し、検証後にファイル転送を自動再開。
- Windows表示モードは再接続猶予中維持する。120秒を過ぎて復帰しなければ元のモードへ復元し、Sessionを閉じる。復元の失敗やHost異常終了にも備える。

## 9. 通信プロトコル — 実測比較の設計 (Q50)

共通プロトコルインターフェースを作成し、以下の2種類を同一のテストハーネスで比較する。

| 比較対象 | 案A | 案B |
|---|---|---|
| 制御/認証 | QUIC/TLS 1.3（Quinn候補） | TCP/TLS 1.3（rustls候補） |
| 映像 | QUIC DATAGRAM | AEAD暗号化UDP |
| 音声 | QUIC DATAGRAMまたは独立ストリーム | AEAD暗号化UDP |
| 入力 | QUIC DATAGRAMと同期制御 | AEAD暗号化UDPと再同期制御 |
| ファイル・クリップボード | QUICの信頼性ストリーム | TCP/TLS上の信頼性ストリーム |

QUICには信頼性ストリームと再送しないDATAGRAMの両方がある。比較において「QUICは必ず低遅延」「TCPは必ず高遅延」と推定しない。等しい解像度・H.264ビットレート、暗号化、回線、測定端末で比較する。

計測項目: 入力→表示のp50/p95/p99遅延、表示FPS、フレーム欠落、映像/音声の遅延、CPU/メモリ、回線使用率、ネットワーク切断からの復帰時間、連続1時間の安定性。1GbEの直結で最初に測定し、遅延/損失を模擬した条件も含める。

失敗条件: High Sierraで依存ライブラリが動作しない、暗号化/認証不備、フレーム破綻を修復できない、安定性が不足する場合。**Q50は比較実施の決定であり、最終的な採用プロトコルは未確定**。

## 10. Windowsインストーラー (Q48)

- 同じインストーラーで「現在のユーザーのみ (Per-user)」「PCの全ユーザー (Per-machine)」を選択可能とする。
- Per-userの既定ディレクトリは `%LOCALAPPDATA%\Programs\LAN Remote Desktop\`。ユーザーのHKCUにアンインストール情報とログイン時起動を登録。通常は管理者権限不要。
- Per-machineの既定ディレクトリは `%ProgramFiles%\LAN Remote Desktop\`。全体のインストールと更新には管理者権限を要求。インストール先/レジストリ/自動起動はHKLMまたは各ユーザーの適切なエントリーを使い分ける。
- **Per-machineを選んでも映像キャプチャ・IME・入力UIはログイン済みのユーザーセッション内で動作させる。** Session 0サービスのみで画面を操作しない。
- NSIS+MultiUserを第一候補とする（最終採否はビルドプロトタイプで確定）。両モードの混在、旧モードからの更新、アンインストール、設定保持を検証。
- Windows Update用別プロセスは、選択したインストールモードの権限境界を守る。ユーザー権限でPer-machineの実行ファイルを改変させない。

## 11. 自動アップデート (Q46, Q47)

- WindowsとMacは**それぞれ**GitHub Releases等のHTTPSエンドポイントからメタデータ/配布物を取得する。Windowsがオフラインの間は更新を保留し、Macから中継しない。通常のLANリモート操作は外部インターネットなしで機能する。
- 更新が見つかったらバックグラウンドで取得・署名/ハッシュ/OS・CPU・プロトコル互換性を検証し、`UPDATE_STAGED` として永続化する。
- **現行セッション中にアップデートのための強制終了をしない。** 次回アプリ起動前に別プロセス（または検証済み更新フレームワークの制御）で適用し、正常起動したら新バージョンとして稼働する。
- Macは10.13対応のSparkle 2.9.6を第一候補とし、希望する「次回起動時適用」の動作を実機で検証。標準機能で厳密に実現できなければ、更新適用用補助プログラムで実現する。現行のSparkle最新系列を無条件に採用しない。
- WindowsはUpdaterヘルパーを別プロセスで実装し、アプリの更新前バックアップ、置換、起動確認、失敗時のロールバックを行う。
- 更新パッケージは公開鍵に対応する秘密鍵で署名する。ダウンロードしたGitHubアーカイブやSHA-256のみを認証手段としない。署名鍵ローテーションは別途設計する。
- Apple Developer IDによるアプリ署名は必須にしないが、署名なしアプリにはGatekeeper制限があるため、ユーザー向けに初回起動手順を案内。Gatekeeper全体の無効化は求めない。

## 12. GitHub ActionsとRelease署名 (Q38–Q40, Q53)

- Publicリポジトリ。標準GitHub-hosted runnerを利用する。
- Push/PR: fmt, clippy, unit tests, protocol tests, Windows/macOSビルドを実施。リリース秘密鍵を渡さない。
- タグ `vX.Y.Z`: release candidate buildを作成し、ビルド物を保存。High Sierra実機の手動試験を経て公開承認を得る。
- Release用GitHub Environment: `release`、Required reviewers、必要ならタグ/ブランチ制限。承認後の署名ジョブだけに署名用環境Secretを提供。リリース作成の権限をそのジョブに限定し、`GITHUB_TOKEN`は他のジョブではread-only。
- Third-party ActionsはフルコミットSHAに固定。fork PR実行コードと署名/公開処理を分離。リリース対象のコミット、テスト済みバイナリのハッシュ、OS最小バージョン、署名対象ファイルの一致を検査する。
- 初期CIランナー候補: Windows `windows-2025`、Intel Mac `macos-15-intel`。macOS Deployment Target=10.13。ランナーは将来変更されるため検査を自動化。
- 出力: Windows用Installer(.exe)、Mac用.app ZIP、SHA-256一覧、署名、リリースノート、更新マニフェスト(appcast)。High SierraのOS依存APIやランタイムは実機手動確認する。
- 署名環境とRelease公開の処理フロー: `tag → build/test → Artifact hash → manual High Sierra smoke test → environment approval → sign → publish`。

## 13. 配布構成とCIレイアウト

```text
lan-remote-desktop/
├─ Cargo.toml
├─ Cargo.lock
├─ rust-toolchain.toml
├─ .github/workflows/
│  ├─ ci.yml
│  ├─ release-candidate.yml
│  └─ release.yml
├─ crates/
│  ├─ common/
│  ├─ protocol/
│  ├─ discovery/
│  ├─ security/
│  ├─ transport/
│  ├─ quality-control/
│  ├─ keyboard-mapping/
│  ├─ clipboard/
│  ├─ file-transfer/
│  └─ updater-core/
├─ host/
│  ├─ app/
│  ├─ capture-dxgi/
│  ├─ video-encoder/
│  ├─ audio-wasapi/
│  ├─ input-win32/
│  ├─ display-manager/
│  ├─ network-helper/
│  └─ updater-helper/
├─ client/
│  ├─ app-appkit/
│  ├─ launch-agent/
│  ├─ decode-videotoolbox/
│  ├─ render-metal/
│  ├─ audio-coreaudio/
│  ├─ input-macos/
│  ├─ network-helper/
│  └─ updater/
├─ packaging/windows/
├─ packaging/macos/
├─ benchmarks/transport/
├─ tests/
└─ docs/
   ├─ requirements-v0.9.md
   ├─ protocol.md
   ├─ security.md
   ├─ installer.md
   └─ testing.md
```

## 14. 実装順序・完了基準

1. **Phase 0**: High Sierra実機でRustの起動、AppKit画面、H.264デコード、GPU描画を確認。CIを整備。
2. **Phase 1**: Ethernet直結・自動探索・初回ペアリングと接続確認。
3. **Phase 2**: DXGI→H.264→VideoToolboxの全画面表示、マウス/キーボード・60fps目標。
4. **Phase 3**: 解像度選択/変更/復元、JIS/USと英数・かな、ゲーム相対入力。
5. **Phase 4**: WASAPI音声、クリップボード、セッション再接続120秒。
6. **Phase 5**: 専用ファイル転送画面、フォルダ、中断/再開/ハッシュ検証。
7. **Phase 6**: ファイルの直接双方向ドラッグ&ドロップ（段階導入）。
8. **Phase 7**: QUICとTLS+UDPのベンチ比較、Adaptive Quality最適化。
9. **Phase 8**: 管理者権限付き固定IP自動設定、ケーブル抜去時の復元。
10. **Phase 9**: 2種類のWindowsインストール、macOS .app、自動更新/署名/ロールバック、Release公開。

受け入れ試験は実機で、1080p/60fpsを達成できるGPU環境での連続表示、キーマッピング、Wi-Fi/LAN分離、120秒復旧、復旧不可時の解像度復元、ファイル転送再開、更新署名偽造拒否、Per-user/Per-machineの権限分離、High Sierraでの更新成功を含める。

## 15. Q1–Q61要件トレーサビリティ

| ID | 確定要件 |
|---|---|
| 1 | MacはmacOS High Sierra 10.13に対応 |
| 2 | HostはWindows 11 Pro、GPUは未確認 |
| 3 | EthernetケーブルによるWindows↔Mac直結 |
| 4 | 映像は端末性能に合わせた最高品質・自動調整 |
| 5 | 通常デスクトップ操作とゲーム/動画の両方を重視 |
| 6 | LAN検知でMacアプリ自動起動、接続開始は確認 |
| 7 | MacモデルとWindows GPUは未確認 |
| 8 | 複数モニターから1画面を選択 |
| 9 | Windowsのシステム音声をMacへ転送 |
| 10 | テキスト・画像クリップボードとファイル転送 |
| 11 | 通常マウス＋ゲーム用相対マウス |
| 12 | Windowsログイン済みの画面のみ |
| 13 | Macの全画面表示 |
| 14 | 画質と遅延を自動で最適化 |
| 15 | Wi-Fiはインターネット用、リモート通信はLANのみ |
| 16 | ドラッグ&ドロップ＋専用ファイル画面 |
| 17 | Windows音声の再生先を設定切替 |
| 18 | 上部メニュー＋ショートカットでモニター切替 |
| 19 | テキスト・画像クリップボードを常時双方向同期 |
| 20 | 初回のみWindowsでも承認、以後Mac確認 |
| 21 | JIS・USキーボードの自動検出・変換 |
| 22 | 接続時Windows解像度を自動変更 |
| 23 | 作業内容で画質と60fps/低遅延を動的最適化 |
| 24 | CommandキーはMac風・Windows風の切替 |
| 25 | 双方向の直接ドラッグ&ドロップを目標 |
| 26 | 信頼済み端末はファイル自動保存、それ以外は確認 |
| 27 | Windows音声のMac専用出力は追加ドライバーなし優先 |
| 28 | 直前に承認済みならLAN再接続で自動復帰 |
| 29 | Windows Installer、Mac .app |
| 30 | Retina解像度は性能に応じて自動選択 |
| 31 | 非対応Windows解像度は近いモード＋Macスケール |
| 32 | 全画面解除はショートカットとツールバー |
| 33 | フォルダ転送と中断後の再開 |
| 34 | 受信先はDownloadsの専用フォルダ |
| 35 | 自動再接続猶予は120秒 |
| 36 | Windows Hostはログイン時起動・トレイ常駐 |
| 37 | GitHub Actions中心の自動ビルド |
| 38 | 公開(Public)リポジトリ |
| 39 | Push/PRでテスト、タグで正式ビルド |
| 40 | High Sierra実機は手動試験 |
| 41 | 個人利用中心・Apple Developer IDなしを許容 |
| 42 | アプリ内の自動アップデート |
| 43 | 同一Host再接続後ファイル自動再開（変更時は確認） |
| 44 | LAN切断後120秒Windows表示モードを維持→復元 |
| 45 | 権限を得た固定IP自動設定機能 |
| 46 | 更新を自動ダウンロードし次回起動で適用 |
| 47 | Windows・MacはGitHubへそれぞれ直接アクセス |
| 48 | Windows Installerはユーザー単位/PC全体を選択 |
| 49 | 固定IPはLANケーブル取り外し時に元へ戻す |
| 50 | QUICとTLS＋UDPを実測で比較選定 |
| 51 | ゲーム相対マウス切替はアプリ検出提案＋ショートカット |
| 52 | 専用ファイル転送を先に実装し、直接D&Dを後で実装 |
| 53 | GitHub Environmentで承認後に署名・リリース公開 |

## 16. 第8段階追加確定事項（Q54–Q61）

- **Q54 一台だけ接続**: 同一Windows PCで操作セッションを持てるMacは全ユーザーを通じて**同時に1台のみ**。PC全体で排他ロックを原子的に確保し、二重接続要求は `BUSY` として拒否する。複数Windowsユーザーが同時ログインしている場合もユーザーセッションごとにHostインスタンスを分け、誤ったセッションへの接続は禁止する。管理用検出情報のみ列挙可能。
- **Q55 前回のHostを優先**: 登録済み公開鍵とユーザーセッションIDを優先して候補表示するが、自動操作開始はしない。複数Host発見時もユーザーへ選択・接続確認を表示し、鍵の変更があれば新規ペアリングを要求する。
- **Q56 総合ベンチマーク評価**: QUICとTLS＋UDPを、遅延p50/p95/p99・画質一致・60fps保持率・CPU使用率・パケット欠落下の復旧・長時間安定性・High Sierra実機互換で比較。暗号化/真正性/再送処理が要件を満たすことを合否の前提とし、ベンチマーク勝者を選ぶ。最終採用プロトコルは**未確定（要検証）**。
- **Q57 Per-machine時も各ユーザー個別Host**: `HKLM\Software\Microsoft\Windows\CurrentVersion\Run`などから各ログインユーザーの対話セッションでHostを起動。共有インストールディレクトリは管理者のみ変更可能。秘密鍵・ペアリング・ログはユーザーごとに分離する。複数ユーザー間で端末鍵を使い回さない。
- **Q58 Stableチャンネル限定**: Stableのみをアップデート対象とし、Beta/Nightly/Pre-releaseを除外。過去版へ意図せずダウングレードしない。通常の更新チェック失敗はオフライン継続。
- **Q59 自動ロールバック**: 新版検証・更新プロセス起動・ヘルスチェック失敗なら旧版バイナリと設定互換状態へ復元する。設定マイグレーションは前方互換/バックアップ/復元を考慮する。再試行ループを防ぐ。
- **Q60 初回権限承認・Ethernetのみ**: 管理者権限付与を受けた限定権限ヘルパーだけが、明示的に選択・記録されたEthernetアダプターのIP設定を変更する。その他のNIC、Wi-Fi、DNS、デフォルトルートは変更しない。要求者認証・コマンド種別固定・設定スナップショット・復元・権限取り消しを必須とする。
- **Q61 ローカル性能ダッシュボード**: RTT、Encode/Decode/Render、表示FPS、フレーム欠落率、実ビットレート、CPU/GPU、メモリ、転送速度、再接続回数をリアルタイム表示。ローカルログはローテーション・保存期限・手動エクスポート対応とする。画面/クリップボード/ファイル本文/端末秘密鍵は記録しない。自動送信はしない。

### Q54–Q61トレーサビリティ

| ID | 確定要件 |
|---|---|
| 54 | 同時接続できるMacは1台 |
| 55 | 前回のWindows Hostを優先し、Macで接続確認 |
| 56 | QUICとTLS＋UDPを遅延・画質・CPU・安定性で総合評価 |
| 57 | PC全体インストールでも各ログインユーザーのHostが起動 |
| 58 | アップデートはStableのみ |
| 59 | 更新失敗時は自動で旧版にロールバック |
| 60 | 初回管理者承認後、指定EthernetのみIP自動管理 |
| 61 | ローカルログと詳細リアルタイムダッシュボード |

## 17. 要件確定後に残る実機・技術検証

- Macの機種/GPU、Windows GPUが未確認。1080p60 / 50msは性能目標であり保証ではない。
- Windows HostとmacOS Clientの全機能は**これから実装する**。この文書や同梱のRust骨組みを完成アプリと混同しない。
- 両方向のOSネイティブ直接ドラッグ&ドロップ、WindowsのMac専用音声出力、Hi Sierra向けSparkleの次回起動適用、GPUエンコード/デコード、ネットワーク特権ヘルパーは独立PoCで可否判断。
- 固定IPの初回承認を永続権限として安全に保持できるかは、macOS 10.13の正式な権限機構との適合性を検証。制御できない場合は都度OS承認を求める安全側動作へフォールバックする。
- macOS 10.13は古いOSであり、インターネット上へリモートサービスを公開しない。アップデート取得HTTPSは別経路とし、オフラインリモート操作を基本とする。

## 18. 技術仕様・実装ファイル

- `docs/tech-stack.md`: 推奨ライブラリ、API、macOS 10.13互換性ゲート
- `docs/protocol.md`: プロトコルメッセージ、映像分割、状態遷移、通信比較
- `docs/security-and-updates.md`: ペアリング、ネットワーク権限、署名付き更新、ロールバック
- `docs/implementation-plan.md`: 実装工程・受入基準・計測
- `docs/codex-master-prompt.md`: エージェント向けマスター指示書
- `.github/workflows/ci.yml`: Push/PR検証ワークフロー（実際のRust骨組みと対応）
- `.github/workflows/release-candidate.yml`: タグ向け候補成果物（配布インストーラーは今後実装）
- `.github/workflows/release.yml`: GitHub Environment保護付きリリース雛形。署名処理実装前は公開失敗を強制する。

## 19. 公式資料（確認日：2026-10-17）

- Rust macOS deployment: https://doc.rust-lang.org/nightly/rustc/platform-support/apple-darwin.html
- Xcode Deployment Targets: https://developer.apple.com/xcode/system-requirements
- GitHub-hosted runners: https://docs.github.com/en/actions/reference/runners/github-hosted-runners
- GitHub Environments: https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/control-deployments
- Sparkle High Sierra compatibility (2.9.6): https://github.com/sparkle-project/Sparkle
- Quinn DATAGRAM: https://docs.rs/quinn/latest/quinn/struct.Connection.html
- Windows DisplayConfig: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setdisplayconfig
