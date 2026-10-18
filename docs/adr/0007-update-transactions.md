# 0007: 独立署名と次回起動時の更新トランザクション

日付: 2026-10-19。対象: Q46/Q47/Q48/Q53。

Sparkleの起動時タイミングに依存せず、Host/Client共通の署名付きパッケージと独立helperで次回起動時の適用を実装する。自動更新のHTTPS経路はEthernetセッションと分離する。

1. OS同梱のcurlを絶対パスから実行。ユーザーcurl設定を無効化し、証明書検証、HTTPS限定redirect、サイズ上限、接続/全体timeoutを適用する。シェルを介さない。
2. Publicリポジトリのlatest ReleaseからCPU別の署名manifestを取得。Ed25519署名を先に検査し、その後にURL・Stable版・OS・CPU・protocol・パス・サイズ・全体/各ファイルhashを検査する。
3. 公開鍵は `packaging/update-public-key.hex` から本体/helperへ埋め込む。秘密鍵はGit外の所有者専用ディレクトリで作成し、リポジトリへ含めない。`cargo run -p lrd-session --example lrd-release-tool -- ...` で鍵生成とオフライン署名が可能。
4. 実行中はprivate stagingのみ。次回の通常起動でhelperへ引き継ぎ、本体終了を待つ。helperは署名と全bytesを再検査し、インストール先にバックアップとjournalを作成してから各ファイルを置換する。
5. 新しい本体のGUI/Hostが初期化したことを、一度限りの256bit tokenと版に結び付くloopback応答で観測する。応答後2秒間の生存を確認し、30秒以内の起動失敗なら子を停止して旧ファイルへ戻す。
6. 中断されたprepared/applying/awaiting-health状態は次回起動で回復する。正常確定した版は自動的に戻さない。失敗回数と版の下限は永続化し、同じ版の起動失敗2回で再適用を拒否する。設定/端末鍵/受信ファイルは適用対象外。

Windowsは版ごとのhelper名を使い、実行中のhelperを自分自身の更新対象に含めない。更新後の本体は新しい版のhelperを使う。macOSは実行中のイメージのatomic renameが可能なため、helperとbundleの署名resource sealを一緒に更新する。

**残りの境界**: 現在はインストール所有者の権限で書き込める配置が対象。Program Filesや管理者所有の.appへの自動昇格・安全な権限分離は未実装であり、書込み権限を緩めず適用を保留する。正式署名公開workflow、管理者経路、実際の2版アプリを使ったOS別の更新試験は未完了。共通エンジンの試験を実際の配布更新成功とは扱わない。

ユーザー指定により、全機能の実装・CI・自動試験を先に進め、Windows/High Sierraでしか確認できない受入試験は最後にまとめる。受入条件そのものは変更しない。
