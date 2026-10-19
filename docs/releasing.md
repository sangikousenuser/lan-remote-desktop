# 候補ビルド・署名・正式公開

Windows x64とIntel macOS 10.13向けの正式配布経路。現在はalphaで、`packaging/release/gates.json` の `implementation_complete` はfalseのまま。管理者所有の配置への更新、直接OLE/File Promiseなどの未完了項目を解消するまで正式公開しない。固定IP helperを同梱するが、実ネットワークの受入合格とは扱わない。実機受入はユーザーの指定により実装・自動試験後にまとめる。

## 候補を作る

`Release Candidate Build`を手動実行、または`vX.Y.Z`タグをpushする。署名秘密鍵を持たないジョブで共通CIを実行し、3OSの通常試験、Windows software MFT→各transport→Mac VT、NSIS、最低OS/依存検査を通す。成功後だけLinuxで署名ツールをコンパイルし、実際のWindows/Intel Mac ZIPから更新対象ファイルを取り出す。

`release-prepared` artifactには次を保存する。

- インストーラー・ZIPの元のbytes
- 更新対象の相対パス・実行属性・OS/CPU・版を定義したrecipeとファイル
- 署名ツールと公開鍵
- 上記全ファイルのSHA-256、commit、run ID、版を持つ`inventory.json`

安全でないパス、symlink、case衝突、重複、256ファイル/512MiB超、欠けたhelper、Macの版/最低OS不一致を拒否する。ZIPを任意パスへ一括展開しない。Windowsの実行中helperは置換せず、新版の版付きhelperを追加する。

## 最後の受入と承認

正式版のCargo版とタグを揃える。候補artifactの実機受入を行い、inventory自体のSHA-256とrun IDを記録する。実装完了ゲートは、残要件を解消したレビューの中で変更する。テストを省略する目的では変更しない。

`Protected Stable Release`を**そのStableタグ**で実行し、候補run IDと以下の形のJSONを渡す。値は実際の候補へ置き換える。確認できていない項目をtrueにしない。

```json
{
  "source_sha": "40桁の対象commit",
  "candidate_run_id": 123,
  "inventory_sha256": "64桁のinventory.jsonのSHA-256",
  "windows_11": true,
  "high_sierra_10_13": true,
  "update_rollback": true,
  "install_modes": true,
  "lan_isolation": true,
  "long_run": true,
  "transport_comparison": true
}
```

このJSONは人が行った受入の申告であり、実機測定の代替ではない。プロセスは同一repo・同一commitの成功した候補workflowだけを認め、inventoryと全bytesを再照合する。実装ゲート・Stableタグ・実機申告のどれかが不足すれば、署名環境の承認前に停止する。

通過後だけGitHub Environment `release` のRequired reviewerへ承認を求める。環境は所有者reviewerと`v*`タグ制限を設定済み。署名の前にも同じ照合を繰り返す。

## 鍵と公開

独立Ed25519鍵を本体/helperに埋め込んだ `packaging/update-public-key.hex` と対応させる。秘密鍵はGit外に保存し、鍵の内容をログやissueへ貼らない。正式公開を設定する段階でだけ、保護されたEnvironment Secret `LRD_SIGNING_KEY_PKCS8_BASE64` にPKCS#8鍵のbase64を設定する。**現在、このsecretは未登録。** PR/通常ビルド/候補ビルドには渡さない。

署名ジョブは候補内の検査済みツールだけを使い、鍵がある状態でCargoやアプリを起動しない。秘密鍵は所有者専用の一時ファイルへ書き、子プロセスの環境から除去し、終了時に削除する。公開鍵との不一致は署名前に拒否する。

各targetのmanifestとpackageを署名後に再検証し、元の配布物・候補inventory・SHA256SUMSとともにdraft Releaseへ追加する。既存Release/assetは上書きしない。公開直前にも署名・全bytes・タグのcommitを再検証し、GitHub APIが返す各assetのSHA-256とサイズが一致した場合だけStableとして公開する。途中失敗はdraftのまま残す。通常CIから正式公開しない。

オフラインの署名/検証CLI（公開は行わない）:

```sh
cargo run --locked -p lrd-session --example lrd-release-tool -- \
  sign PRIVATE_PK8 RECIPE_JSON SOURCE_ROOT OUTPUT_DIRECTORY packaging/update-public-key.hex
cargo run --locked -p lrd-session --example lrd-release-tool -- \
  verify MANIFEST_JSON PACKAGE_LRDPKG packaging/update-public-key.hex
```

CIは一時的な別鍵・fixtureファイルで署名→検証→改ざん拒否→異なる公開鍵の拒否を実行する。本番鍵や正式Releaseを使わない。公開APIの試験はmockであり、実際の公開成功とは区別する。

参照: [GitHub release asset digest](https://docs.github.com/en/rest/releases/assets)。
