# ADR 0004 — Metal優先とCPU互換経路

日付: 2026-10-17。状態: 開発Macで検証、High Sierra検証待ち。

MTKViewとCIContextのMetal経路でH.264デコード済pixel bufferを描画する。
同じフレームを再描画した回数をFPSへ加算しない。異なる世代のGPUコマンド完了だけを数える。
これはディスプレイ光学測定ではなく、リモート入力→表示の遅延でもない。

2026-10-19更新: 通常モードはVideoToolboxへhardware優先を要求し、非対応/使用中はsoftwareへ復帰する。以前のhardware必須による初期化失敗を、古いIntel Macで接続不能のままにしないために変更した。hardware選択の実測値を表示し、softwareでGPU性能要件を達成したとは扱わない。hardwareの継続的な復号失敗ではsessionを破棄してsoftwareで作り直し、keyframeを要求する。
[Apple hardware decoder enable/disable](https://developer.apple.com/documentation/videotoolbox/kvtvideodecoderspecification_enablehardwareacceleratedvideodecoder)。

Metal非対応GPUや互換性試験ではCore Image CPU描画へ切り替える。`--software`で再現可能。
`--software`はhardware decodeも明示的に無効化する。Metal command失敗時は入力を解放してCPU描画へ切り替える。入力の初回取得・鮮度判定は復号数ではなく描画完了世代に従い、描画のない状態で入力を有効化しない。GPU decodeを使用したかは実測プロパティに従って報告する。
OpenGL contextの追加管理を避けるため、Phase 0では仕様の代替候補OpenGLよりCore Image CPUを選んだ。
CPU経路で1080p60を保証しない。High SierraのGPU/CPU両経路の確認が必要。
