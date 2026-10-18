# ADR 0006 — 接続機能を持つalpha版

日付: 2026-10-18。状態: 開発用の暫定実装。Q50の最終Transport選定ではない。

## 通信と認証

物理Ethernetを列挙し、送信元IPv4とインターフェースを拘束する。MacはIP_BOUND_IF、WindowsはIP_UNICAST_IF、UDP受信はIP_IFLIST/IP_ADD_IFLISTを使用する。Windowsのweak-host/forwarding構成は拒否する。探索は選択NICのサブネットbroadcast、入力/映像/音声/転送はTCP/TLS 1.3。NICのMAC/IP/リンク変化を監視して切断する。Wi-FiやDNSへのfallbackは持たない。

端末証明書はP-256、rustls/ringのCertificateVerifyを相互検証する。初回の自己署名証明書はPKIの信頼先としては扱わず、TLS exporterに結び付けた6桁コードを両端で照合し、双方の許可を受けてからSHA-256 fingerprintを保存する。アプリの機能を開始するのはこのゲートの後。既知の接続もMacで毎回承認し、初回状態が片側だけの場合も両端でコードを出す。

alphaのwireは `major:u8, kind:u8, length:u32BE, payload`。制御64KiB、映像/転送8MiB、クリップボード内容4MiB、入力JSON8KiBを上限とする。`lrd-protocol`の24byte案は別の設計用codecで、ライブ通信との相互互換を主張しない。正式wireの統合はTransport比較と共に行う。

TLSの所有者は1つ。ノンブロッキングsocketとrustlsの64KiB出力上限を使い、平文/暗号文の部分書き込みと読み込みを交互に進める。大きい双方向転送で両端がwrite_allに停止することを避ける。優先制御64件、bulk2件、アプリ受信8件。bulkも8件の優先メッセージ後に処理する。ping/echoでRTTを測定し5秒の無受信で切断する。

QUIC DATAGRAMとAEAD UDPはまだ実装していない。TLS/TCPの暫定経路を性能上の勝者と扱わない。

## 映像、音声、入力

DXGI CPU staging→BGRA/NV12変換→非同期hardware H.264 MFTを優先する。設定/処理失敗、初期出力の停止時にMicrosoft software MFTへfallbackする。現状はGPU surface共有のzero-copy経路ではない。Annex B/AVCC/SPS/PPSを正規化し、MacでVTへ供給する。画面が静止していても初期フレームと再同期用フレームを送る。VT/描画queueの超過時はkeyframeを要求する。通常はHW decode、明示したsoftware modeとCPU描画fallbackを用意する。

現時点の音声codecはbounded Float32 PCM。WASAPI共有loopbackをCoreAudio AudioQueueで再生する。入力装置の多chはステレオへ縮約する。Opusは未実装。Macだけの出力はWindows muteでloopbackを破壊しないよう制限を案内する。

SendInputのJIS/US、修飾キー合成、IME_ON/OFF、絶対/相対座標を使用。入力はsessionごとのepochと単調sequenceで検査する。Macのフォーカス離脱、明示解放、切断で保持したキー/ボタンを解放する。最初の映像まで入力を取得せず、3秒の映像停止時も解放する。UI threadからの入力keepaliveが2秒途絶えた場合はHostも押下を解放する。UIPI、secure desktop、Raw Inputゲームは制約を維持する。

HostはWin32のGlobal named mutexを専用所有threadに保持する。別ユーザーを含め操作は1台のみ。猶予中もleaseを保持し、同じ証明書とメモリー内の256bit ticketで120秒以内だけ復帰する。再初期化失敗では元の期限を延長しない。ticketは同じ承認済み論理セッションで維持し、通信断でCONFIGが失われても復帰不能にならないようにする。TLS接続鍵と入力epochは毎回新しい。

## 表示設定と転送

モニター列挙とOSが返したモードだけを提示する。変更前にoriginal/appliedと位置/回転をprivate journalへ保存する。切断/猶予満了で復元し、次回起動でも照合する。現在の設定がappliedと一致する時だけ復元し、ローカルの変更を上書きしない。

転送元をmanifest化し、1MiBチャンクと全体SHA-256、変更時刻、Transfer IDを保存する。受信はcap-stdの受信ルート内に固定し、絶対/親/ADS/予約名/大文字小文字衝突、symlink、過大manifestを拒否する。検証済みチャンクをsyncしてから状態を保存する。再開時もディスクのチャンクを再hashする。完成は全体検証後のhard linkで既存ファイルを上書きせず配置する。クラッシュ後の同一hash完成先は成功扱い。転送元変更時は再選択を要求する。

専用UIへのOSファイルdropを実装する。Explorerの任意位置へ直接リモートファイルをOLE提供するものではなく、受信は固定Downloadsへ行う。

## 配布と更新

NSIS MultiUserで両インストールモードを生成する。captureはログイン済みユーザーのプロセスで実行し、Session 0サービスにしない。選択NICはMACで保持する。MacのLaunchAgentも明示したEthernetのみを監視し、同じリンクで拒否通知を繰り返さない。

更新validatorは、Ed25519署名を先に検査してからStable version、CPU/OS/protocol、HTTPS URL、個別/全体SHA-256、サイズ、パスを検査する。private stagingで最後にUPDATE_STAGEDを書く。これは公開鍵を埋め込む実アプリupdaterや更新適用helperへまだ統合していない。自動取得/置換/rollbackを実装したと扱わない。Stable公開workflowは停止を維持する。

## 検証の限界

ローカルTLS往復、承認拒否、変更証明書拒否、同時大容量送受信、TLSから実VideoToolboxまでの復号、1GiB超転送エンジンの中断再開、署名偽造拒否は自動試験で確認する。Windows/Intel High Sierraの実LAN動作、HW encode使用率、光学遅延、1時間安定性は別の実機試験が必要。
