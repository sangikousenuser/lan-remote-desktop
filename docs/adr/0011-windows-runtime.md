# 0011: Windows配布物のruntimeとPE依存検査

日付: 2026-10-19。対象: Q29/Q48。

Windows MSVC targetは `.cargo/config.toml` で `crt-static` を有効にする。Windows 11の利用先へ外部Visual C++ runtimeの導入を追加要求しないため、Hostと更新helperへ静的リンクする。GNU cross buildも別途、実ファイルの依存を検査する。

配布前にPE32+のx64・section/RVA・通常importとdelay importを上限付きでparseし、Windows system DLL/API set以外へ依存する実行ファイルを拒否する。vcruntime140、msvcp140、libgcc_s_seh等を隠れた前提にしない。Hostとhelperの両方を検査し、結果をCIへ残す。これはOSのGPU driver/MFTや実機での起動受入を代替しない。

参照: [Rust CRT linkage](https://doc.rust-lang.org/reference/linkage.html#static-and-dynamic-c-runtimes)、[Microsoft PE format](https://learn.microsoft.com/en-us/windows/win32/debug/pe-format)。
