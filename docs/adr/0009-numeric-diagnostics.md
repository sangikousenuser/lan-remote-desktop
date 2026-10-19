# 0009: 内容を持たないローカル性能ログ

日付: 2026-10-19。対象: Q61。

`lrd-session::diagnostics`は固定された数値/booleanの型だけを記録する。任意文字列、パス、IP、端末fingerprint、入力、画面、clipboard、転送内容を格納するフィールドを持たせない。Host/Clientの承認済み接続からのみ自動記録する。

- 実装済み: RTT、capture処理/encode/同期decode/render完了時間、capture/decode/render FPS、codecのhardware選択、送受信byte、queue/drop/error、プロセスCPU、resident/peak memory。CPU 100%は1コア相当。GPU使用率や入力→表示遅延を推定して表示しない。
- `transport_bitrate_bps`はtransportへ渡したアプリデータ量（media headerを含む）の差分。暗号/TCP/IP overheadを含む物理回線bitrateではない。codecの要求値は別の`encoder_target_bitrate_bps`。
- 数値ログは非同期・bounded queue（32件）。ディスクが遅い/満杯でもセッションを待たせず、取りこぼし件数を表示する。
- 最新snapshot・観測時刻・プロセス資源値はqueueへ渡す前に更新する。ログwriterの停止や書き込み遅延でdashboardが止まったり、古い観測が後から新しい時刻になることを防ぐ。
- 既定7日、合計100MiB、segment 1MiB。所有者専用ディレクトリ/ファイルを使い、期限/容量超過の管理対象segmentだけを削除する。無関係のファイルやsymlinkは削除/追跡しない。
- macOSは映像欄へCPU/RAMを追加、Windowsは通知領域の接続情報へ数値を表示する。両端に「性能ログを書き出す…」を追加する。
- 書き出し時にJSONLを数値型へ再parseし、不明フィールド・文字列・破損末尾を除外する。最大100MiBで打ち切り、除外数/打切りflagを付ける。アプリの秘密鍵/設定領域への書き出しは拒否する。

設定を変える場合は、各ユーザーのアプリデータに`log-policy.json`を所有者だけ読める状態で置く。

```json
{"max_bytes":104857600,"retention_days":7,"segment_bytes":1048576}
```

保持1〜31日、総量32KiB〜1GiB、segment 4KiB〜16MiB、想定segment数1024以下に制限する。WindowsのアプリデータはLocalAppData、MacはLibrary/Application Support内。設定なしなら既定値。

テスト: own-process resource取得、期限/容量/回転、無関係ファイル保持、NaN/負値/不正上限拒否、文字列/不明データ/破損末尾をexportしないこと。

captureのDXGI待機時間は処理時間に含めない。VTは非同期/temporal flagsを使わず、callbackまで完了した時間を計測する。renderはCPU描画開始→Metal command完了、CPU fallbackは描画呼出しの時間。10.15以降だけguard付きでGPU command時間も表示する。光学表示時間やGPU使用率とは区別する。

受信側は可視状態・interval・世代・decoded/rendered・工程時間・error/drop差分だけのtyped feedbackを送る。Hostは同じinput_epoch・3秒以内のfeedbackのみ採用する。隠れたウィンドウの低FPSを負荷と誤認しない。RTT/encode/queueに加え受信工程の過負荷/欠落を下方調整へ使う。最大4096画素のBGRA差分sampleはメモリ内だけで変化率を計算し、画面内容をログへ記録しない。変化/入力が少ない時は高bitrateを保って30fpsを優先し、活動再開2回/静止安定5回のヒステリシスで切り替える。

`file_send_bytes_per_second`は送出queueへ受理したファイル本文、`file_receive_bytes_per_second`はhash検証後の本文のbytes/s。送信完了の保証値とは区別する。復帰回数は同じ承認済みセッションへの復帰成功時だけ加算する。

参照: [Apple VT decode flags](https://developer.apple.com/documentation/videotoolbox/vtdecompressionsessiondecodeframe(_:samplebuffer:flags:framerefcon:infoflagsout:))。GPU使用率のOS別実装と実機での調整値検証は残る。GUIはビルド検査済みだが、開発Macはロック中のため新しい表示/保存panelの操作確認は保留する。
