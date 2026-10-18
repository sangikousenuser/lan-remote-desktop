# 0008: Q50の比較用Transportと共通ハーネス

日付: 2026-10-19。対象: Q50。最終方式は未選定。

`lrd-transport`に2候補を実装する。ユーザーアプリの既定live経路は、現在も従来のTLS/TCPである。以下の候補は共通ハーネスで動作し、本体の映像/入力/音声/再開状態への接続が残っている。

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

報告するp50/p95/p99は、フレーム全体の**往復・再構成時間**。入力→表示遅延、描画FPS、GPU codec性能ではない。実機の方式選定には本体統合、同一1GbEの実映像/音声/入力、両順序・予備運転、損失/遅延、再接続、1時間安定試験が必要。loopbackだけから勝者を選ばず、JSONの`selected_winner`はnullとする。

初回開発Macの300frame + 8MiB bulk試験では両候補とも全frameのecho/再構成とbulk一致を確認した。数値は `docs/validation/transport-loopback-alpha.json` に保存する。物理LANの測定ではない。

参照: [Quinn](https://docs.rs/quinn/0.11.12/quinn/)、[Tokio](https://docs.rs/tokio/1.53.3/tokio/)、[ring AEAD](https://docs.rs/ring/0.17.14/ring/aead/index.html)。Socket2をTokioが必要とする0.6.5へ固定更新し、Cargo.lockで依存を固定する。
