# ADR 0001 — ツールチェーンと互換性ゲート

日付: 2026-10-17。状態: 開発用採用、High Sierra実機承認待ち。

Rust 2021 / Rust 1.99.0を固定し、Cargo.lockを管理する。既存のstable追従は、将来の最低OS変更やlint差分を再現できないためやめる。
この開発MacはApple Silicon / macOS 27.0.1。Intel x86_64アプリをクロスビルドし、最低OS 10.13をMach-Oで検査する。
公式Rust資料はdeployment targetの指定方法を説明しているが、設定だけでは実機動作を証明できない。
https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html

macOS 27 SDKのCoreMedia.tbdには、旧OS向けのリンク先を`@rpath/libswiftCoreMedia.dylib`へ変更する指定があり、実際に生成物へ混入した。
これを依存検査で拒否した。インストール済み26.5 SDKでCoreMediaのC ABIへ直接リンクする。
Appleのpython3起動ラッパーがSDKROOTを設定するため、デフォルト27 SDKを検知して26.5を選び、選択SDKを明示出力する。
`package.py --sdk`指定は優先する。build.rsもSDKROOTを追跡し、リンクのsyslibrootを明示してCargoキャッシュによるSDK不一致を防ぐ。
SDKを改変せず、古いOSにないdylibを同梱して無理に通すこともしない。

CIはMSVC Windows、Intel macOS、Linuxで検証する設定。実機試験は別ゲート。ここではGitHub CI自体を実行していない。
