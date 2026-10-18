# ADR 0002 — AppKitを小さいObjective-C bridgeから呼び出す

日付: 2026-10-17。状態: Phase 0採用。

要件Q1/Q13/Q32に従い、AppKitのネイティブウィンドウと全画面メニューを作る。
WebViewベースのUIとSwiftランタイム依存を増やさず、Rustのmainから同期C ABIを1つだけ呼ぶ。
このPhaseではObjective-CがCF/ObjC参照とGPUオブジェクトを所有し、Rustへ借用画像ポインターを渡さない。
Rust/Objective-C双方で所有する設計より、解放責務とスレッド境界が明確になる。

UI/Metal描画はmain thread、AVAssetReaderとVideoToolboxの投入はserial queue。共有pixel bufferはNSLockで保護し、retainしてから描画する。
UIの停止時はtimerをcancelしてqueueをdrainし、古いdecoderの完了通知を新しい再生へ混入させない。
10.13より新しいAPIのunguarded availabilityはコンパイルエラーとする。
SDK上でdeprecatedとなった古いAVAssetReader用APIは、10.13対応に必要なため警告を限定的に抑制する。

ネイティブコードを含めたビルドはローカルで成功。High SierraのAppKit起動は未検証。
ネットワーク接続画面を偽装せず、この段階のUIはローカル映像テストと明記する。
