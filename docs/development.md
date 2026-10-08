# 開発・検証

ビルド、CLI、テスト、ベンチマーク、GUIのスモークテスト。使い方は[使い方](user-guide.md)、データ構造と内部処理は[内部形式](internal-format.md)、表示の参照元は[Potreeメモ](potree-rendering-notes.md)を参照。

ローカルの入力ファイル、作業プロジェクト（`work-data/`）、ビルド成果物はリポジトリに含めない。プロジェクトを書き換えるコマンドは、コピーに対して実行する。

## ビルドと配布

- WindowsのRust MSVC環境とC++ Build Toolsが必要。Rustの版は`rust-toolchain.toml`で固定している（現在1.99.0、egui 0.36の要件は1.95以上）。
- 配布物は`cargo build -p geemil-desktop --release`で作る`target/release/geemil-desktop.exe`の1ファイル。CLIが必要なら`geemil.exe`も添える。`.cargo/config.toml`でCランタイムを静的リンクしているので、Visual C++再頒布可能パッケージは不要。
- GUIアプリとしてビルドするため、コンソールウィンドウは開かない。スモークテストの出力は、起動したターミナルかリダイレクト先へ出る。
- 描画は既定でDirectX 12。`WGPU_BACKEND`環境変数で変えられる。日本語フォントはWindowsのメイリオ／游ゴシックを使い、同梱しない。

## テストと静的検査

### GitHub Actions

[CIワークフロー](../.github/workflows/ci.yml)はWindows Server 2025のx64ランナーで実行する。Rustと検査用コンポーネントは`rust-toolchain.toml`に従い、Cargoのテスト・検査・ビルドには`--locked`を指定する。

| きっかけ | 実行内容 |
| --- | --- |
| `main`宛てのPR | フォーマット検査、ワークスペースのテスト、全ターゲットのclippy |
| `main`へのプッシュ（PRのマージを含む） | 上記の検査に成功したら、GUI・CLIを`--release`でビルドして成果物を保存 |
| タグのプッシュ | 上記の検査とReleaseビルドに成功したら、タグ名を含むZIPを作り、GitHub Releaseへ自動添付 |
| Actionsの「Run workflow」 | 検査を実行。`main`なら成果物も作成。タグを指定して実行した場合はReleaseへの添付も実行 |

同じPR・ブランチに新しい更新が来ると、古い実行はキャンセルする。Rustの依存関係とビルドキャッシュは再利用し、キャッシュの保存は`main`だけで行う。

`main`の成果物は`geemil-workbench-windows-x64-<コミットSHA>`という名前で30日間保存する。中身は`geemil-desktop.exe`・`geemil.exe`・`LICENSE`・`README.md`・`docs/`・`BUILD.txt`・`SHA256SUMS`。`BUILD.txt`には元のコミットとActions実行のURLが入る。[Actions](https://github.com/wagadao-y/geemil-workbench/actions/workflows/ci.yml)の成功した実行からArtifactsをダウンロードする（GitHubへのログインが必要）。

### タグからのリリース

リリースするコミットにタグを付けてプッシュする。タグ名は先頭を英数字とし、英数字・`.`・`_`・`-`を使う（例: `v0.0.2`）。CI設定を含むコミットに付けること。ローカルでタグを付けただけではCIは起動しない。

```powershell
git tag v0.0.2
git push origin v0.0.2
```

タグが指すコミットをビルドし、`geemil-workbench-v0.0.2-windows-x64.zip`をReleaseへ添付する。ZIPの中身は`main`の成果物と同じで、`BUILD.txt`にはタグ名も記録する。タグ向けのZIPはActionsにも`release-windows-x64`として30日間保存する。

Releaseが未作成なら、タグ名をタイトルとして変更内容を自動生成し、ZIPを添付して公開する。既存のReleaseがあればZIPだけを追加し、タイトル・説明・下書き状態などはそのまま保つ。同名のZIPが既に添付されている場合はスキップするので、CIを再実行しても添付済みのファイルは上書きしない。

検査・ビルドのジョブは読み取り権限で実行し、その成功後に動くRelease用ジョブだけに`contents: write`を付与する。認証には自動発行される`GITHUB_TOKEN`を使い、追加のシークレット設定は不要。

CIの検査はGPUを必要としないテストが対象。GUIのスモークテストと大規模データの性能計測は、下記の手順で別途実行する。

### ローカル

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

結合テストは[crates/core/tests/](../crates/core/tests/)にある。

## CLI

```powershell
cargo run -p geemil-core --release -- import work-data/trimble Trimble_StSulpice-Cloud-50mm.e57
cargo run -p geemil-core --release -- inspect work-data/trimble
cargo run -p geemil-core --release -- subsample work-data/trimble-copy 0.05
cargo run -p geemil-core --release -- noise work-data/trimble-copy 0.1 4
cargo run -p geemil-core --release -- sor work-data/trimble-copy 6 1 0.5
cargo run -p geemil-core --release -- subsample-merged work-data/trimble-copy 0.05
cargo run -p geemil-core --release -- reduce-overlap work-data/trimble-copy 0.1
cargo run -p geemil-core --release -- export work-data/trimble work-data/trimble-roundtrip.e57
cargo run -p geemil-core --release -- export-las work-data/trimble work-data/trimble.laz
cargo run -p geemil-core -- demo work-data/demo.e57   # 小さな検証用E57を生成する
cargo run -p geemil-core --release -- import work-data/manitou-pano photo.jpg   # JPEG/PNGはパノラマ写真として取り込む
```

フィルターは、判定した点を「Subsampled」（間引き）か「Noise」（ノイズ除去・SOR）のレイヤーへ移す。`inspect`はレイヤーごとの点数も表示する。

### E57の書き戻しの照合

```powershell
cargo run -p geemil-core --release --example verify_e57 -- Trimble_StSulpice-Cloud-50mm.e57 work-data/trimble-roundtrip.e57
```

未編集で書き戻したE57を元のファイルと照合する。点数、schema、scan pose、image pose、画像対応、投影パラメーター、点順序によらない数値属性のハッシュ集計、画像・マスクのハッシュを比べる。全点はメモリに載せない。ハッシュの照合は、暗号学的な完全一致の証明ではない。

ローカルの実データで照合したもの:

| データ | スキャン | 点数 | 画像 | 確認 |
| --- | ---: | ---: | ---: | --- |
| manitouNoInvalidPoints.e57 | 5 | 1,095,702 | 5 | 元の点属性、スキャン・画像pose、投影・画像データ |
| Trimble_StSulpice-Cloud-50mm.e57 | 5 | 8,484,455 | 0 | RGBを含む元の点属性、スキャンpose、DirectX 12表示 |

## ベンチマーク

`crates/core/examples/`にある。結果は計測した時点のもので、データ・ストレージ・OSキャッシュで変わる。1億点での最新の通し検証は[2026-10-04の性能検証](performance-review-20261004.md)を参照。

### 合成データの作成（make_synthetic）

地上レーザー風の合成データ（スキャナー位置から角度一様に光線を出し、地面・円形の壁・箱に当たった点。近いほど密）をLASで作る。引数はスキャン数と1スキャンの点数。

```powershell
cargo run -p geemil-core --release --example make_synthetic -- work-data/syn100m 10 10000000
cargo run -p geemil-core --release -- import work-data/syn100m-p (Get-ChildItem work-data/syn100m/*.las)
```

回転方向の手がかりが少ないシーンなので、ICPの精度評価には向かない（ずれなしから始めても隅で最大1m動く）。精度は実データのmanitouで確認する。

### 合成パノラマ（make_panorama）

プロジェクトの点を、指定した位置から見た正距円筒図法のJPEGに描く。正解の位置が分かっているパノラマとして、配置の確認に使う。引数はプロジェクト、出力、撮影位置X Y Z、方位（度、+XからZ軸回りに左回り）、幅（既定4096）。プロジェクトだけを指定すると、スキャナー位置と範囲を表示する。

```powershell
cargo run -p geemil-core --release --example make_panorama -- work-data/manitou
cargo run -p geemil-core --release --example make_panorama -- work-data/manitou work-data/pano-truth.jpg 8.0 -4.3 0.3 30 6144
```

manitouのコピーに6144×3072の合成パノラマを取り込み、下のスモークテストで正解の位置から8組の対応点を作ると、位置と方位は0.0001 mm・0.001°未満の差で戻った。12288×6144のJPEGは、デコード・縮小版の作成・テクスチャ転送まで約1秒で表示した（smokeプロファイル、2026-10-07）。

### 連続処理（workflow_bench）

新しいプロジェクトを作り、取り込み、2cm間引き、Undo/Redo 20往復、ノイズ除去（半径5cm・4点未満）、指定数の視点のCPU表示更新、LAS/LAZ書き出しを続けて実行する。入力と既存のプロジェクトは変更しない。プロセスメモリとstaging容量の観測ピーク（50msごと、およびOSの累積ピーク）と、表示更新のp50/p95/p99を出力する。GPU描画の時間は含まない。

```powershell
cargo run -p geemil-core --release --example workflow_bench -- work-data/bench-current work-data/syn100m 1200
```

### 取り込み（import_bench）

工程ごとの経過時間と取り込み全体の時間を測る。出力先は未作成のフォルダーを指定する。第3引数はワーカー数（0は自動）、第4引数は変換バッファーの予算（MiB）、第5引数は省略可能な比較用プロジェクト。工程は重なって動くので、表示される時間は各工程の単独のCPU時間ではない。

```powershell
cargo run -p geemil-core --release --example import_bench -- Trimble_StSulpice-Cloud-50mm.e57 work-data/bench-8 8 256
```

### 表示の読み込み（view_bench）

同じ周辺で視点を少しずつ回し、ノード選択と読み込みのCPU時間を測る。GPU転送と描画は含まない。`view_bench PROJECT [BUDGET] [WORKERS] [REVISION]`で、WORKERSは0で自動、1で直列。REVISIONは保存済みリビジョンの名前。

```powershell
cargo run -p geemil-core --release --example view_bench -- work-data/trimble
```

### スキャン数・編集回数への依存（scale_bench）

点数ではなく、スキャン数や編集回数で増える処理を測る。初回は指定したLASフォルダーからプロジェクトを作り、スキャンをフォルダーに分け、指定回数の小さな範囲選択で編集を積む（2回目以降は作成済みのプロジェクトを使う）。スキャン一覧、ツリーの集計、ノード選択（キャッシュなし・あり）、ノード読み込みの時間を出力する。

```powershell
cargo run -p geemil-core --release --example make_synthetic -- work-data/syn-scale-las 300 30000
cargo run -p geemil-core --release --example scale_bench -- work-data/syn-scale-proj work-data/syn-scale-las 20 200
```

### 範囲選択の移動（selection_bench）

初期の全体表示で、画面中央の正方形の範囲を「Deleted」レイヤーへ移す。第2引数は正方形の半幅（0.5で画面全体）、第3引数は奥行き（m、`inf`で無制限）、第4引数に`outside`を指定すると範囲外（切り出し）を測る。プロジェクトを書き換える。

```powershell
cargo run -p geemil-core --release --example selection_bench -- work-data/trimble-copy 0.05 0.5
```

### ICP（align_bench）

各スキャンに既知のずれを与え、他のスキャンに合わせ直して戻り具合を見る。引数は開始距離（既定0.5）、ずれの大きさ（既定1で約1.8°と15cm）、サンプル点数（既定6万）、距離ごとの最大反復回数（既定50）、終了距離（既定0.02）。

```powershell
cargo run -p geemil-core --release --example align_bench -- work-data/manitou 0.5 1
# 終了距離を開始距離と同じにすると、その距離だけで合わせる
cargo run -p geemil-core --release --example align_bench -- work-data/manitou 0.5 1 60000 50 0.5
```

manitou（5スキャン）では、開始0.5m・終了0.02mで、重なり45〜69%の4スキャンがスキャン範囲の隅で0.5〜1.1cm以内に戻った（1スキャン約0.6秒、2026-10-05）。重なり11%のスキャンは合わない。ずれなしから始めても元の位置合わせから1〜4cm動くため、これが比較の下限になる。

### 全体の最適化（global_bench）

最初のスキャン以外に、後のスキャンほど大きいずれを与え、最初のスキャンを固定して全体を合わせ直す。引数はずれの大きさ（既定1で、1スキャンごとに約0.1°と1cmずつ増える）、開始距離（既定0.1）、終了距離（既定0.02）、1スキャンのサンプル点数（既定5万）。プロジェクトは一時フォルダーへ複製して扱い、元は変更しない。

```powershell
cargo run -p geemil-core --release --example global_bench -- work-data/manitou 1
```

manitouでは、ずれを与えても与えなくても同じ解（差は約1cm以内）に収まり、組ごとのRMSの平均は6.8mmから5.6mmに下がった（約5秒）。実データには正解がないので、隅の誤差は元の位置合わせとの差を表す。

## GUIのスモークテスト

`--smoke-test PNG`で起動し、表示の読み込みを待ってから画面をPNGに保存して終了する。プロジェクトを指定しなければ起動画面を保存する。

動作確認には`smoke`プロファイル（最適化あり・LTOなし・インクリメンタル）を使う。1ファイルの変更後の再ビルドが`release`の約46秒から約6秒になる。速度を比べるベンチマークと長時間計測は、配布物と同じ`--release`で行う。

| オプション | 動作 |
| --- | --- |
| `--smoke-orbit` | 2秒間カメラを回転・平行移動し、移動中にもノードの更新が完了することを確認する |
| `--smoke-orbit-seconds N` | 回転・平行移動の時間（対象から離れない経路を使う） |
| `--smoke-budget N` | 描画点数を変える |
| `--smoke-colors` | 既知の12色を表示し、スクリーンショットのpixel値を入力と照合する |
| `--smoke-select MODE` | 起動1秒後に画面中央を矩形選択し、赤表示の点数と基準点を出力する。`inside`（奥行き0.5m）、`inside-all`（奥行き無制限）、`outside`（範囲外） |
| `--smoke-dialog NAME` | ダイアログを開いた状態を保存する。`revisions`・`properties`・`shortcuts`・`new-project`・`cleanup`・`scatter`・`bulk-rename`・`save-as`・`export`・`export-las`・`subsample`・`noise`・`moving` |
| `--smoke-script STEPS` | 操作を順に実行する（下表） |

```powershell
cargo run -p geemil-desktop --profile smoke -- work-data/trimble --smoke-test work-data/preview.png --smoke-orbit
cargo run -p geemil-desktop --profile smoke -- --smoke-test work-data/colors.png --smoke-colors
cargo run -p geemil-desktop --profile smoke -- work-data/trimble --smoke-test work-data/select.png --smoke-select outside
cargo run -p geemil-desktop --profile smoke -- work-data/manitou --smoke-test work-data/revisions.png --smoke-dialog revisions
cargo run -p geemil-desktop --profile smoke -- work-data/manitou-copy --smoke-test work-data/flow.png --smoke-select inside-all --smoke-script exclude,undo,redo,folder,save,remove,measure
```

### --smoke-script の操作

カンマ区切りで指定する。各操作の後に、スキャン数・フォルダー数・レイヤーごとの点数・リビジョン数・未保存の有無・元に戻す／やり直すの可否を出力する。各行は操作を始めた時点の状態で、バックグラウンド処理（`job true`）の結果は次の行に出る。プロジェクトを書き換える。

| 分類 | 操作 |
| --- | --- |
| 編集と履歴 | `exclude`（選択した点を「削除」へ移す）、`undo`、`redo`、`save`、`folder`、`remove` |
| 計測と表示 | `measure`、`ortho`（平行投影の上面図）、`color-height`、`color-scan`、`adaptive`（点サイズを適応に）、`focus`／`focus-folder`（最初のスキャン／フォルダーにカメラを寄せる）、`zoom`（カメラ距離を1/8に） |
| 変換 | `preview`（最初のスキャンの変換を入力し、動いた表示点数を出力）、`apply-transform`、`transform`／`transform-folder`（最初のスキャン／フォルダーを選び「移動・回転」ツールにする）、`place-handles`（画面中央に取っ手を置き、前後の位置を出力） |
| フィルター | `subsample`（5cm）、`noise`（半径0.1m・4点未満）、`sor`（6点・1σ）、`subsample-merged`（まとめて5cm）、`moving`（動体除去、既定値） |
| 位置合わせ | `align-icp`（最初のスキャンをICPでプレビュー）、`align-pairs`（一致する4組の対応点で合わせ、動かないことを確認）、`align-global`、`align-list`（一覧の件数と全体最適化の前後のRMSを出力）、`align-switch`／`align-leave`（別のスキャンの選択／ツールの切り替えで、未適用の確認が出たかを出力）、`align-apply` |
| パノラマ | `panorama`（最初のパノラマ写真で配置ツールを開く）、`panorama-pairs:X:Y:Z:HEADING`（その位置・方位で撮ったとして、表示中の点から方位の8方向に1点ずつ対応点を作って解き、正解との差を出力）、`panorama-apply`、`panorama-link`（左右の視点の連動と点の重ね合わせ）、`panorama-view:YAW:PITCH:FOV`（写真の表示方向と画角、度。パノラマの表示中はその表示に効く）、`panorama-view:flat`（全景）、`tree-fold`（プロジェクトツリーを閉じる）、`navigate`（カメラ操作に切り替える）、`tour`（最初の配置済みパノラマを中央の画面で表示）、`tour-exit`、`hide-panorama`（最初のパノラマのマーカーを非表示） |
| ボックス | `box`（中央の高さに2mの水平スライスを置いてハイライトし、箱の中の点数を出力）、`box-resize`（傾けた箱でサイズ変更の取っ手を表示）、`box-crop`（箱の外を「削除」へ移す） |
| レイヤー | `show-layers`（すべて表示）、`solo-layer`（最も新しいレイヤーだけ表示）、`restore`（最も新しいレイヤーの点を「点群」へ戻す） |

## GUIの長時間計測

PowerShell 7で実行する。`scripts/measure-gui.ps1`は、RAMとWindowsのGPUプロセスメモリカウンターを約500msごとに記録し、PNG・ログ・metrics JSONを保存する。GPUカウンターが取れなければnullになる。フレーム間隔はUI callbackの観測値で、GPU fenceや画面提示を直接測った値ではない。既存のPNGは上書きしない。`-SmokeScript`で編集するときは、コピーしたプロジェクトに対して使う。

```powershell
cargo build -p geemil-desktop --release
.\scripts\measure-gui.ps1 -Project work-data/bench-current -OutputPrefix work-data/bench-gui -OrbitSeconds 120
```
