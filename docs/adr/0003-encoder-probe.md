# ADR 0003 — DXGI取得とMedia FoundationによるローカルH.264出力

日付: 2026-10-17。状態: PoC実装、Windows実機検証待ち。正式encoder backendは未決定。

Windows API bindingをwindows-rs 0.58.0へ固定した。モニター/アダプターを番号で選択し、DXGI Desktop Duplicationで取得する。
AcquireNextFrameは100msでタイムアウト、ReleaseFrame/UnmapはRAIIで行い、エラー時もリソースを返す。
ACCESS_LOST時には再生成を試みる。ロック画面/権限外のデスクトップを回避して操作する機能は作らない。
https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/desktop-dup-api

最初のPoCはCPU staging textureへBGRAをコピーし、Sink WriterでH.264 MP4を保存する。
ハードウェアMFTを列挙し、Sink WriterにはHW変換を許可するが、列挙結果だけでGPU encodeの成功とはしない。
https://learn.microsoft.com/en-us/windows/win32/api/mfapi/nf-mfapi-mftenumex

低遅延の本実装はD3D11 surface共有・NV12変換・hardware async MFT・キーフレーム要求/codec config取り出しが必要。
ローカルMP4を作れることとリアルタイムencoderの完成を区別する。未回転かつ偶数寸法の画面だけをPoC対象とし、未対応条件はエラー。
配布するMinGW GNU版とCIのMSVC版の両方で実機確認が必要。
