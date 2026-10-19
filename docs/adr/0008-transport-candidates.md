# 0008: Q50の比較用Transportと共通ハーネス

日付: 2026-10-19。対象: Q50。最終方式は未選定。

`lrd-transport`に2候補を実装する。ユーザーアプリの既定live経路は、現在も従来のTLS/TCPである。両候補を本体の承認・再接続処理へ接続した。Macの「通信方式」メニューから次回接続に使用する方式を選べる。実Ethernetの総合比較と最終選定は残っている。

- A: Quinn 0.11.12 / Tokio 1.53.3。相互TLS 1.3、既存と同じ証明書検査と両端承認、exporter確認コード、QUIC DATAGRAMと信頼性stream。0-RTTと接続migrationを無効化。アプリデータはAES-256-GCMに限定する。QUIC Initialに必要なAES-128-GCMは独立した標準のinitial suiteを指定する。
- B: rustls 0.23.46の相互TLS/承認、信頼性TLS stream、TLS exporterから方向別に導いた鍵を使うAES-256-GCM UDP。標準のring AEADを使用し、暗号primitiveは実装しない。24byteの認証headerに版・kind・session・sequence・lengthを含める。nonceは方向別keyごとの単調64bit sequenceで、wrap前に停止する。1024 packetのreplay bitmapはtag成功後にだけ進める。
- 両候補ともアプリデータは承認後の型からのみ送信する。物理EthernetのIP/NIC拘束、同一subnet、link/IP/Windows isolation監視を使う。ネットワーク列挙はpacketごとに行わず、250msのwatcherで失効させる。
- loopbackはCLIで明示した診断scope。Ethernet不在時にloopbackやWi-Fiを自動選択しない。
- 同じ58,253byteの合成H.264 keyframe、共通media codec/reassembler、同じ1100byteのアプリdatagram上限、同じ間隔、同じ信頼性bulk echoを使用する。QUIC MTUは1200へ固定する。

## 再現

```sh
cargo run --locked --release -p lrd-transport --example transport-compare -- \
  --loopback --samples 300 --output target/transport-loopback.json
```

実Ethernet用の比較CLIもある。両端で同じcandidateを指定し、表示されたコードを比較して承認する。例のINDEXとIP:PORTは実際の値へ置き換える。

```sh
cargo run --locked --release -p lrd-transport --example transport-compare -- --interfaces
# 受信側: 待受IP:portを表示（合成データ専用。画面操作はしない）
cargo run --locked --release -p lrd-transport --example transport-compare -- --listen --interface INDEX --transport quic
# 測定側:
cargo run --locked --release -p lrd-transport --example transport-compare -- --connect IP:PORT --interface INDEX --transport quic --samples 300 --output target/quic.json
```

`--transport hybrid`でも同じ条件で繰り返す。Firewallが必要なら、この診断用実行ファイル・選択Ethernet・表示port・同一subnetだけを許可する。ハーネスがFirewall/IPを変更することはない。

## 数値の限界

報告するp50/p95/p99は、フレーム全体の**往復・再構成時間**。入力→表示遅延、描画FPS、GPU codec性能ではない。実機の方式選定には同一1GbEの実映像/音声/入力、両順序・予備運転、損失/遅延、再接続、1時間安定試験が必要。loopbackだけから勝者を選ばず、JSONの`selected_winner`はnullとする。

初回開発Macの300frame + 8MiB bulk試験では両候補とも全frameのecho/再構成とbulk一致を確認した。数値は `docs/validation/transport-loopback-alpha.json` に保存する。物理LANの測定ではない。

参照: [Quinn](https://docs.rs/quinn/0.11.12/quinn/)、[Tokio](https://docs.rs/tokio/1.53.3/tokio/)、[ring AEAD](https://docs.rs/ring/0.17.14/ring/aead/index.html)。Socket2をTokioが必要とする0.6.5へ固定更新し、Cargo.lockで依存を固定する。

## 本体への接続

HELLO・120秒のlease・両端承認は従来のTLSで行う。既定のTCPでは追加フィールドを省略し、従来の成功HELLO/ACKと互換。選択時だけtransportを明示し、暗黙のフォールバックは行わない。Hybridは承認済みTLSのexporterからUDP鍵を導く。QUICの待受portはそのTLS内で交換し、同じ証明書pin・登録済み鍵を再検証した上で、元のTLS exporterのSHA-256をQUIC内で相互照合して別セッションへの誤接続を拒否する。0-RTT/migrationは使用しない。

映像と音声は36byte media headerと最大1100byteのdatagramへ分割する。入力・キー/ボタン解放・構成・clipboard・ファイルは順序と到達性を保つstreamを使う。入力のunreliable化は順序保証とボタン状態同期を備えるまで行わない。media session_idには新しいinput_epochを使用し、モニター変更前・再接続前の映像を拒否する。100ms期限・最大8frame再構成・bounded queueを維持し、映像欠落時は500msを上限頻度にkeyframeを要求する。手動切断はCLOSEをflush/FINした後に終了する。

HostはTCP port+2のUDPを使用し、探索はport+1。Firewallスクリプトは実行ファイル・選択Ethernet・同一subnet・32組のこれらのportだけを許可する。Mac側UDPは選択NICの動的port。TCP/UDPともリンク/IP/prefixの変更で停止し、再接続の再探索は元のMACと証明書/Windows sessionに限定する。

自動試験は3方式でH.264・音声payload・キー解放・双方向512KiB bulk・構成世代切替・CLOSEを検証する。Macでは各候補を通したH.264が実VideoToolboxで復号されることも検証する。これは実ディスプレイでの描画や物理LANでの性能合格を意味しない。
