# ADR 0004 — Metal優先、明示的CPU描画互換モード

日付: 2026-10-17。状態: 開発Macで検証、High Sierra検証待ち。

MTKViewとCIContextのMetal経路でH.264デコード済pixel bufferを描画する。
同じフレームを再描画した回数をFPSへ加算しない。異なる世代のGPUコマンド完了だけを数える。
これはディスプレイ光学測定ではなく、リモート入力→表示の遅延でもない。

通常モードはVideoToolboxへハードウェアデコーダーを必須として要求する。非対応/使用中は明示エラー。
https://developer.apple.com/documentation/videotoolbox/kvtvideodecoderspecification_requirehardwareacceleratedvideodecoder

Metal非対応GPUや互換性試験ではCore Image CPU描画へ切り替える。`--software`で再現可能。
CPU描画でもVideoToolboxがGPU decodeを利用できれば利用する。GPU decodeを使用したかは実測プロパティに従って報告する。
OpenGL contextの追加管理を避けるため、Phase 0では仕様の代替候補OpenGLよりCore Image CPUを選んだ。
CPU経路で1080p60を保証しない。High SierraのGPU/CPU両経路の確認が必要。
