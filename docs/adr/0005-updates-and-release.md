# ADR 0005 — Sparkleと正式公開のゲートは未通過

日付: 2026-10-17。状態: 保留。

要件Q46/Q47/Q53/Q58/Q59に従う。Sparkle 2.9.6は候補のままとし、この版へは同梱しない。
High Sierra実機で署名確認・次回起動時適用・旧版保全が検証できていないため、採用済みと扱わない。
https://github.com/sparkle-project/Sparkle

macOS arm64の起動に必要なad-hoc code signatureは、Developer ID署名や更新の真正性を示す署名ではない。
ZIPのSHA-256も更新認証の代用にはならない。現時点の配布はローカル映像PoCとしてだけ扱う。
Release workflowはfail-closedのまま、タグのcandidateはprobe成果物として明示する。
次の起動前UpdaterやWindows installerを未検証の状態で有効化しない。

QUIC/TLS+UDPの採用判断も測定まで保留する。無暗号の代替Transportを先に公開しない。
