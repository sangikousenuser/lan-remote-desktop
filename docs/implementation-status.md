# 実装状況と検証記録

更新: 2026-10-19。バージョン: **0.9.0-alpha.1**。

ローカル映像PoCから、Ethernet上の認証済み画面・入力・音声・ファイル転送を行う実装へ進めました。
**仕様全体は未完成です。** 実装済みのコードと、対象実機での合格は区別します。
ユーザーからWindowsでの先行テスト済みの報告を受けていますが、新規セッション機能の実行結果・数値はまだ受領していません。High Sierra実機は利用不可です。

## 実装

| 項目 | 実装内容 | まだ必要なこと |
|---|---|---|
| Ethernet/探索 | 物理有線を選択、送受信NIC/IPv4拘束、同一subnet限定、UDP broadcast、NIC監視、Windows weak-host/forwarding拒否 | 両OS/複数NICでの実パケット確認。DHCP/Link-LocalそのものはOSの機能 |
| 端末認証 | TLS 1.3相互署名、exporter確認コード、両端初回承認、fingerprint保存、Mac毎回承認 | 実機で拒否/鍵変更/非対称登録を操作確認 |
| Host排他 | PC全体Global mutex、所有thread、ユーザーセッション識別、猶予中のlease保持 | Windows複数ログイン/別Mac競合。TCP/探索UDPを32組の排他的なユーザー別スロットで確保。非対話セッションは探索へ応答しない |
| 映像 | DXGI staging、NV12、非同期HW H.264 MFT優先＋software fallback、NAL配信、VT/Metal/CPU描画、keyframe要求、静止時の再同期 | GPU surface共有、実HW MFT互換、60fps/低遅延・色・長時間試験 |
| 入力 | SendInput、JIS/US物理検出、かなON/英数OFF、Command変換、絶対/相対マウス、緊急解放、世代/順序検査 | Windows IME/配列/ゲーム別試験。secure desktop/UIPI制約 |
| 再接続 | 120秒、同じ証明書・同じWindows session・secret ticket、TLS鍵/入力世代更新、手動切断抑止、初期化失敗で期限維持、同じMACのNICで再探索、IP/port変更へ追従 | 実際の抜線、NICアドレス変更、121秒、復帰前後の入力試験 |
| モニター/解像度 | 複数モニター選択、対応モード列挙、Mac物理/論理画素表示、比率維持、変更前journal、条件付き復元、猶予後/再起動時照合 | Windows表示ドライバーごとの変更/強制終了/手動変更試験 |
| 品質/指標 | encode負荷/RTT/queueのヒステリシス、fps/bitrate制御、受信側工程/欠落/可視状態のfeedback、画面変化/入力頻度、工程時間/転送速度/復帰回数のdashboard、CPU/RAM、7日/100MiBの数値ログと検査済みexport | 実入力→光学表示遅延、GPU使用率のOS別取得、実LANでの調整値検証 |
| 音声 | WASAPI loopback、PCM多ch縮約、CoreAudio、bounded queue、Windowsのみ/両方、Macのみ制限案内 | Opus、デバイス変更/音映像同期/長時間実測。Windows muteで代用しない |
| Clipboard | 双方向UTF8/PNG、SHA-256、サイズ/画素制限、反射抑制、停止 | Windows/macOSの各アプリで実動作確認 |
| 転送 | 専用UI、双方向ファイル/フォルダー、階層/複数選択、進捗/取消、SHA-256、1MiB chunk、永続manifestと再開、capability受信、非上書き完成配置 | 実LANの両方向1GiB超、遅いディスク、アプリ再起動の受入試験。不要な部分データの整理UI |
| Drag & Drop | Finder→Mac転送/映像画面、Explorer→Windows専用画面から固定受信先へ転送 | 任意Explorer/Finder位置への直接リモートOLE/File Promise連携 |
| 常駐/配布 | Windows tray/専用転送画面、同一NICで待受再構築、NSIS Per-user/Per-machine、任意のログイン時起動、Mac LaunchAgent登録/解除、.app/ZIP | 両Windowsモードのインストール/アンインストール実機、High Sierra LaunchAgent |
| 更新 | HTTPS取得、埋込みEd25519署名、Stable/CPU/OS/protocol/hash/パス検査、private staging、次回起動helper、backup/journal、起動応答/生存監視、失敗復元、失敗版blocklist、候補bytes/実機申告照合→Environment承認→署名/検証→draft hash照合/公開workflow | 管理者所有の配置への権限分離・実際の2版アプリでの更新試験。Stableは未公開、公開ゲートfalse |
| Q50 | QUIC DATAGRAM/TLS+AEAD UDPの両候補、相互認証/両端承認、本体の映像/音声/入力/転送/再開へ接続、通信方式メニュー、暗号/replay/世代検査、共通比較CLI | 実Ethernet性能/損失/長時間と最終方式選定。live既定はTLS/TCP |
| 固定IP | OSのDHCP/Link-Localを利用。Firewall限定設定スクリプトを同梱 | 狭い管理者helper、snapshot/rollback、衝突検出、両端ローカルウィザード |
| CI/正式公開 | Public repo、3OS fmt/clippy/tests/buildとWindows NSIS/Intel .app生成成功、alpha取得可能、正式公開を拒否するゲート | 追加変更のActions検証、実機ゲート、署名公開workflowの完成 |

## ローカルで確認したこと

開発端末: Mac17,9 / Apple M5 Pro、macOS 27.0.1。Rust 1.99.0。Windows x64はMinGW cross build、Intel Mac版はcross build + Rosettaです。

- Rust: common 9件、protocol 14件、session 25件、Mac TLS→VideoToolbox 1件、合計49件の通常テスト。
- セッション試験: 相互TLS・確認コード一致、片側拒否時の両方の登録禁止、別証明書pin拒否、双方向の256KiB×8パケットを8KiB socket bufferで同時送受信、期限/世代/サイズ/パス/署名異常。
- 別途、ignored指定の1GiB超テストを実行済み。1GiB+123bytesを512チャンクで中断し、Receiverを作り直して残りを受信、全体SHA-256一致。182.47秒。この結果は転送エンジンのローカル試験であり実LAN試験ではありません。
- Macの実アプリ: Ethernet不在の案内、専用転送画面、ローカル映像、全画面バー、Control+Command+Fによる解除。
- `cargo fmt` / 両対象のclippy、通常テスト、PE/Mach-Oビルド、Intel/arm64最低OS/依存検査、Mac GPU/CPUと異常系smoke、NSIS生成、依存監査の結果は `docs/validation/` に保存します。
- rustlsを0.23.46へ更新。検出された旧版の公開advisoryを解消しました。更新用の独立鍵を生成し、公開鍵だけをソースに登録します。秘密鍵はGit外の所有者専用ディレクトリに保存しています。公開Releaseはまだありません。

現在のMacにはIPv4設定済みEthernetが検出されません。対象のWindows機への実接続は行えていません。
High Sierraでの互換性は最低OSのロードコマンド検査やRosettaでは代替できません。
対応GPUでの1080p60/50ms目標を達成したとする実測値はありません。

## 次の受入ゲート

新しいWindows ZIPの `Start-Host.ps1` とMacアプリで実際に接続し、初回承認→映像/入力→音声/clipboard→抜線復帰→モニター変更→両方向転送を確認します。[手順](manual-testing.md)に結果欄を定義しています。
ユーザー指定により、実機受入は全機能の実装と自動試験後にまとめて行います。Q50、固定IP helper、更新の管理者経路・公開workflow、直接OLE転送の残り実装を実機試験待ちにせず進めます。正式版としての公開ゲートは解除しません。

## GitHub公開

公開先は https://github.com/sangikousenuser/lan-remote-desktop 。ユーザーの指定によりPublicリポジトリへソースを登録済みです。実行ファイル、端末鍵、個人用の診断JSONはGitに登録しません。Windowsでの新しい接続試験は「まだ試せない」との回答のため未実施のままです。コミット `74aeb9e` の3OS CIとWindows/Intel Macのパッケージ生成が成功しました: https://github.com/sangikousenuser/lan-remote-desktop/actions/runs/39140663687 。これはHigh Sierraや実LAN/GPUの合格ではありません。

更新機能追加後のローカル検証: 通常59件成功（session 35、common 9、protocol 14、Mac TLS→VT 1）。ignored 2件のうち1件は起動監視の子プロセス用fixtureで、親テストから実行します。別の1件は前回実施した1GiB転送試験です。OS別CIへWindowsの実software MFT encode試験も追加しました。

性能診断追加後のローカル通常テストは71件成功（common 9、protocol 14、session 41、transport 6、Mac TLS→VT 1）。Windows版は125%/150%等のDPI仮想化を避けるよう初期化を追加。GPU encoderが途中から出力しなくなった場合もsoftwareへ復帰する。画面ロック中の描画再試験は未合格として保留しており、復号300・描画0の結果を成功扱いにしない。

本体Transport統合後: 通常78件成功（common 9、protocol 14、session 43、transport 9、Mac codec 3）。Hybrid/QUICともnative adapterの制御/世代/手動終了と実VT復号を確認。Windows由来fixtureのCI試験も両候補経由のVT復号へ拡張する。

性能feedback追加後は通常82件成功。VTは非同期・並べ替えflagsを使わない同期呼出しで復号時間を計測。描画時間はCPU開始からMetal command完了（CPU fallbackは描画呼出し）までで、光学表示時間ではない。10.15以降だけavailability guard付きでGPU command時間も計測する。GPU使用率は取得できなければ未知のまま表示する。
