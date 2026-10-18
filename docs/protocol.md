# LAN Remote Desktop v0.9 — 通信プロトコル詳細設計 (Draft wire protocol v1)

**注意**: 以下は実装可能な設計案であり、実測・監査・interop検証前の最終採用プロトコルではない。Q50・Q56のとおりQUICとTLS+UDPを比較して選ぶ。プロトコル共通のアプリ層を先に実装する。

## 0. 通信境界

- Ethernet直結の選択インターフェースへbind。Wi-Fiへ自動迂回しない。更新用のHTTPSだけ例外。
- mDNS/DNS-SD (候補 `_lanremoted._udp.local`) を指定インターフェースで発見。mDNS失敗時に限定したUDP discovery。未認証発見応答は**絶対に信頼しない**。
- 初回ペアリングはWindows/Mac双方のローカル承認と一致確認コード（例6桁）を必要とする。ペアリングは認証済みの鍵交換結果だけ保存する。認証済み端末公開鍵の一致を通信成立の条件とする。
- HostとWindowsログインセッションは区別する。多ユーザーインストールでも**ユーザーセッションごと**に秘密鍵・承認・映像取得を独立させる。
- Windows PC全体で同時操作できるMacは1台。ログイン済みユーザーごとにHostは別プロセスだが、PC全体の安全な排他ロックを認証後に確保し二台目は`BUSY`。

## 1. トランスポートアダプター

```rust
pub trait TransportAdapter {
    // 実装はasync traitか同期Facadeのどちらかに統一する
    // connect(peer: &PeerIdentity, iface: InterfaceId) -> Session
    // send_reliable(channel: ReliableChannel, msg: &[u8])
    // send_unreliable(channel: DatagramChannel, msg: &[u8])
    // on_network_change(), stats(), close()
}
```

案A: Quinn QUIC (TLS 1.3)。Control/Clipboard/Fileは信頼ストリーム、Video/Audio/Input移動はDATAGRAM。押下/解放と入力状態再同期は信頼ストリームも使う。

案B: rustls TCP/TLS 1.3（Control/Clipboard/File）＋認証付き暗号化UDP（Video/Audio/Input）。TLS握手に結びつけて鍵を導出し、AEAD nonceの一意性・anti-replay・鍵ローテーション・鍵失効を実装する。**素のUDPに無暗号データを流さない。**

共通要件: 制御ストリームに大容量ファイル転送を詰め込まず、フロー制御と優先度を実装。メディアが遅れたとき古い映像データを捨て、鍵/ペアリングを検証する。QUIC DATAGRAMはMTUに収まる必要がある。

## 2. Reliable control frame（Network byte order）

| オフセット | 型 | 内容 |
|---|---|---|
| 0 | u32 | length: 後続フレーム長、上限1MiB |
| 4 | u8 | protocol_major = 1 |
| 5 | u8 | protocol_minor = 0 |
| 6 | u16 | message_type |
| 8 | u64 | request_id（要求/応答の関連付け） |
| 16 | u64 | session_id（認証後割当） |
| 24 | bytes | 検証済みPayload（serde表現は後に固定） |

- `length` はヘッダー以降のバイト数。巨大payloadはFile用ストリームへ分離する。
- バイト順はBig Endian。フレーム解析前に長さ/バージョン/状態を検証。
- アプリケーション層の署名・暗号化は下位の認証済みTransport上で実施。制御メッセージの内側に鍵や秘密情報を不用意に含めない。
- 内部境界の値は `u32` / `u64` でも適切に上限値検証する（オーバーフロー防止）。

主なMessage Types（暫定ID・予約済）：

| ID | 型 | 内容 |
|---|---|---|
| 0x0001 | HELLO | protocol ranges, role, software version |
| 0x0002 | CAPABILITIES | encoder/decoder, resolution, max_fps, audio |
| 0x0003 | PAIRING_REQUEST | approval challenge (secret keyは含めない) |
| 0x0004 | PAIRING_RESULT | 確認済み鍵識別子 / 結果 |
| 0x0005 | SESSION_START | 許可されたユーザーセッション指定 |
| 0x0006 | SESSION_ACCEPT | authenticated session_id, input_epoch |
| 0x0007 | SESSION_BUSY | 他Clientにより占有 |
| 0x0008 | HEARTBEAT | timestamp, last_acked_seq |
| 0x0009 | SESSION_CLOSE | reason, manual flag |
| 0x000A | VIDEO_CONFIG | monitor, codec, geometry, fps, bitrate |
| 0x000B | VIDEO_KEYFRAME_REQUEST | decoder invalidation |
| 0x000C | INPUT_KEY_STATE | 全キー状態・epoch同期 |
| 0x000D | DISPLAY_MODES | Windowsの対応する表示モード一覧 |
| 0x000E | DISPLAY_SET | validate, apply, restore token |
| 0x000F | CLIPBOARD_OFFER | mime, origin, content hash, size |
| 0x0010 | FILE_MANIFEST | transfer id, relative paths, chunk hashes |
| 0x0011 | FILE_RESUME_REQUEST | 完了チャンクのbitmap/ハッシュ |
| 0x0012 | FILE_TRANSFER_STATUS | progress, cancelled, error |
| 0x0013 | TELEMETRY_SNAPSHOT | ローカル画面表示向け匿名数値値 |
| 0x0014 | AUDIO_CONFIG | codec, sample rate, channels |
| 0x0015 | INPUT_MODE | absolute / relative, proposed mode |

### プロトコルネゴシエーション

`major` の非互換なら接続拒否、同一majorでminorのfeature bitmapを交渉して未知機能を無効化。サーバーが古いクライアントを誤って操作させない。更新中のバージョン違いでは両アプリの更新を案内する。

## 3. Media datagram（Big Endian、サイズ例）

| オフセット | 型 | 内容 |
|---|---|---|
| 0 | u8 | protocol_major |
| 1 | u8 | kind: video=1, audio=2, input=3 |
| 2 | u16 | flags: keyframe, eofなど |
| 4 | u64 | session_id |
| 12 | u32 | stream_id (monitor ID等) |
| 16 | u64 | sequence/frame_id |
| 24 | u16 | fragment_index |
| 26 | u16 | fragment_count |
| 28 | u64 | monotonic_capture_time_us |
| 36 | bytes | payload |

- データグラム最大サイズは接続時に`max_datagram_size`で検出。開始目標は**各パケット約1200バイト以下**とし、実際のヘッダー/暗号化オーバーヘッドに合わせる。固定1200が全経路で成立するとは仮定しない。
- H.264の1フレームは複数のfragmentへ分割。再構成はframe_idで管理、重複/逆順/欠落を処理。ハード上限例：フレーム8MiB、断片数8192、滞留フレーム数8（いずれも初期案でありベンチ後に確定）。
- 待ち時間が大きい古い不完全フレームは破棄し、依存関係破綻ならキーフレーム再要求。タイムスタンプは単調時計基準、端末間の絶対時刻差を直接レイテンシと見なさない。片道遅延は時計同期または外部計測で評価。
- 入力イベントは`input_epoch`と増加sequenceで重複/過去セッションを拒否。キー状態変更（押下・解放）は信頼ストリームでも確認し、切断したらHost側でリセットする。

## 4. 接続状態機械

`IDLE → DISCOVERED → PAIRING(初回) / APPROVAL(通常) → AUTHENTICATING → NEGOTIATING → CONNECTED → RECONNECTING → CONNECTED | CLOSED`

- Macで通常接続の承認が下りるまでHostのDesktopへの操作は禁止。
- `CONNECTED`以外は`SendInput`を実行しない。
- `RECONNECTING`開始と同時にキー状態解放・入力廃棄。新規セッション鍵、新しい`input_epoch`を要求。
- 120秒以内・同一Peer公開鍵・手動切断なしのみ接続確認なしで復帰。120秒切れで元の画面解像度復元。
- リンク切断時の固定IP設定復元は再接続猶予と**別の状態管理**：Ethernetリンクダウンで設定復元。IPが戻るまで一時的にセッション再接続不可でも安全を優先。
- Per-machine導入でもユーザーセッションごとのHost間で鍵や操作権限を共用しない。

## 5. 機能別伝送

**映像**: 独立カーソル表示。選択モニター1画面。H.264初期仕様=8bit 4:2:0、低遅延。目標1080p/60fps、画質優先モードでは静止文字の精細さを優先。高解像度・HEVCは互換確認後。

**音声**: WASAPI→Opus→Core Audio、packet timestampとjitter buffer。入力音声/マイクは非対象。音声再生先の切り替えはWindowsドライバー不要を優先。

**クリップボード**: `clipboard_origin_id + update_seq + content_hash` により反響送信を抑止。text/plain UTF-8, image/png等のみ許可。サイズ上限・内容形式の検証。

**ファイル**: `transfer_id(UUID) / manifest / path / size / modification_id / per-chunk sha256 / final sha256`。標準チャンク初期案は1MiB。検証済みチャンクbitmapをローカル永続化。renameは検証済み完成時のみ、パストラバーサル・Symlink・ADS・特殊ファイルは安全に取り扱う。同一信頼済みHost・転送元無変更なら自動再開、変更時は確認。

## 6. QUIC 対 TLS+UDP 公平ベンチマーク

- 最低: 1GbE直結、1080p60同一H.264エンコーダー/ビットレート、同一Client、同一解像度、同一暗号認証条件、同一ビルド最適化。
- 観測: p50/p95/p99入力→画面光学遅延(可能なら高速度カメラ)、画面表示fps、frame age、オーディオ遅延、ドロップ/フリーズ率、CPU/GPU利用、ネットワーク使用率、1時間連続安定性、切断再接続時間。
- 追加負荷: 背景ファイル転送と映像を同時利用、可変遅延/損失パターン、LAN瞬断、再送によるHead-of-Line影響。
- **ゲート**: 暗号認証成立、High Sierraで正常起動、実装の完全性、停止からの安全復帰を満たすこと。未達の方式は選ばない。
- **総合評価**: 各指標を同条件で正規化し「映像/入力遅延35%、安定60fps20%、CPU/メモリ15%、損失復旧20%、実装保守10%」を比較用の暫定重みとする。実機データを集めて採用前に合意する。どちらが勝つかは**未確定**。

## 7. パケットパーサ安全性

- 指定最大長超過、未定義version、未知の必須type、invalid enum、index >= count、0 fragment、トランザクション状態外の呼び出しを拒否。
- 所有権のないメモリ・未検証の長さをFFIに渡さない。fuzzing（cargo-fuzz）、property tests、再接続故障注入、転送パス処理の悪意テストを実行。
- 認証済みTransportとアプリ権限の実接続は**未実装**。Rustコードは上限付きcontrol/media codec、stream framing、重複/逆順/欠損/期限付き再構成を含む。ネットワークへ直接接続してよい実装ではない。
