# 開発・検証

ビルドの詳細、内部処理の概要、CLI、テスト、ベンチマークと実測値。使い方は[使い方](user-guide.md)、内部形式は[internal-format.md](internal-format.md)を参照。

## ビルドと配布

WindowsのRust MSVC環境とC++ Build Toolsが必要。Rustの版はリポジトリの`rust-toolchain.toml`で固定している（現在1.99.0、egui 0.36の要件は1.95以上）。rustupが自動で取得する。初回ビルドは依存ライブラリをダウンロードする。

WindowsではDirectX 12を既定とする。`WGPU_BACKEND`環境変数でバックエンドを指定できる。画面の日本語フォントはWindowsのメイリオ／游ゴシックを使用し、配布物には同梱しない。

配布物は`cargo build -p geemil-desktop --release`で作る`target/release/geemil-desktop.exe`の1ファイルだけでよい。`.cargo/config.toml`でCランタイムを静的リンクしているため、Visual C++再頒布可能パッケージは不要で、Windows標準以外のDLLに依存しない。CLIが必要な場合は`geemil.exe`も添える。`target`のそれ以外の中身はビルド用の中間生成物で、配布しない。GUIアプリとしてビルドするため、起動してもコンソールウィンドウは開かない。スモークテストの出力は、リダイレクトしていればそこへ、ターミナルから直接実行した場合はそのターミナルへ出る。

## 内部処理の概要

- 原解像度チャンクは最大65,536点・最大32 MiB。全点をRAMに展開せず、インデックス構築・範囲判定・書き出しをチャンク単位で行う。
- 原解像度はチャンクごと、表示用8分木はノードごとにbyte shuffle + Zstd level 1で可逆圧縮する。圧縮して増えるブロックは無圧縮で保存する。元の属性のbit列、点ID、点ごとのレイヤーとの対応は変わらない。
- 取り込み時のE57点属性変換、チャンクの圧縮と、チャンクの下の表示用8分木の構築を並列処理する。ワーカー数はCPU数−1（最小1・最大8）、処理中と書き込み待ちの変換バッファーは既定256 MiBの予約枠で制限する。アプリ全体のRAM上限ではない。空間分割、読み出し、書き込み、チャンクより上の表示用8分木の構築は直列。詳細は[内部形式の並列取り込み](internal-format.md#並列取り込み)を参照。
- 点のRGBはsRGBとして表示する。経路は[Potreeメモ](potree-rendering-notes.md#rgbの表示経路)を参照。alphaは色変換しない。
- 点群は、処理用の原解像度チャンク（範囲選択・フィルター・位置合わせ・書き出しに使う）と、表示用の8分木の2つで持つ。表示用8分木はPotree 2と同じ足し算型で、全点がどれか1つのノードに1回だけ入り、上のノードから順に足すと密度が上がる。座標・色・元の点への参照だけを持ち、Trimbleの848万点で点データ203 MBに対し111 MB。
- 描画上限の既定は200万点で、画面から調整できる。表示はPotreeと同じく、画面上で大きいノードから描画点数に収まるまで選び、キャッシュにないノードだけを読み込みながら少しずつ表示する。読み込んだノードはGPUに常駐させ、カメラを動かしても点を減らさず、新しく見えた部分だけを足す。展開済みノードのキャッシュとGPUの常駐は、どちらも描画点数の2倍の点まで（キャッシュは1点40バイト、GPUは1点20バイト）。キャッシュはスキャンのローカル座標で持つため、位置合わせの適用ではディスクから読み直さない（点のレイヤーやレイヤーの表示が変わると破棄する）。GPUではスキャンごとの変換を描画時に掛けるため、変換の入力・適用・元に戻すは次の読み込みを待たずに表示へ反映する。
- 非表示点のマスクと空ノードの管理領域も表示キャッシュの予算に含め、点と共通のLRUで追い出す。ノード／チャンクのメタデータはRAMに保持する。RAM/VRAMに応じた表示予算の自動調整は今後の実装。
- フィルターの処理メモリ予算は既定768 MiB。画面で128〜4,096 MiBを指定でき、次回も保持する。予算の1/4をチャンクキャッシュへ配分し、残りとチャンクの展開サイズから並列数を決める（自動は最大16）。ノイズ除去とSORの近傍探索は、チャンクごとのkd-treeを予算内で使い回し、各点はまず自分のチャンクを探して、探索の球がチャンクの外に出る点だけ隣のチャンクを探す。チャンクの周りの探索範囲の点をまとめて集めないので、探索範囲がチャンクより大きい密なスキャンでも時間が探索範囲にほぼ依らない。結果は以前の方式と一致する。結果もワーカー数の2倍のチャンクごとにラベルへ書く。メタデータ、表示、allocator、圧縮ライブラリ内部の領域などは別に使用するため、アプリ全体のRAM上限ではない。

## CLIと検証

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
cargo run -p geemil-core --release --example verify_e57 -- Trimble_StSulpice-Cloud-50mm.e57 work-data/trimble-roundtrip.e57
```

CLIのフィルターは、判定した点を「Subsampled」（間引き）か「Noise」（ノイズ除去・SOR）のレイヤーへ移す。`inspect`はレイヤーごとの点数も表示する。

`verify_e57`は未編集の書き戻しを検証するためのツール。点順序によらない数値属性のハッシュ集計、点数、schema、scan pose、image pose、画像対応、投影パラメーター、画像・マスクのハッシュを照合する。全点をメモリに格納しない。ハッシュ照合は暗号学的な完全一致証明ではない。

小さな検証用E57を生成するコマンドもある。

```powershell
cargo run -p geemil-core -- demo work-data/demo.e57
```

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

36件の結合テストと10件の単体テストで、複数スキャンと複数画像の保持、pose、元ファイルなしの再開、原解像度での奥行き選択、範囲内・範囲外・奥行き無制限の移動と表示プレビューの一致、レイヤー間の点の移動・一部とレイヤー全体の戻し・レイヤーの削除・非表示レイヤーを処理と書き出しから外すこと、未保存の作業状態と保存・分岐・切り替え、元に戻すためのスナップショット、フォルダーの変換の合成と移動時の補正、リビジョンの削除、未使用データのクリーンアップ、処理途中のキャンセル、LAS/LAZ取り込み、ノイズ除去・SOR・ボクセル間引き（スキャンごと・まとめて）・3Dボックス選択の総当たり結果との一致とチャンク分割への非依存、LAS/LAZ書き出しでの強度・色・変換・非表示レイヤーの除外・座標系の保持とスキャン別出力、ICPと対応点による既知のずれの復元（フォルダー内のスキャン・フォルダー自体）と重なりがない場合のエラー、少ない描画枠での複数スキャン表示、球面座標・scaled integer・無効点・大きな整数属性を検証する。加えて、他の形式バージョンの拒否、キャッシュ再利用、リビジョンによる無効化、浮動小数点のbit保持、圧縮データの破損検出、ワーカー処理の重なりと出力順序、完了結果のメモリ予約、ワーカーエラー、1／4ワーカーでの出力一致、インデックス構築中の中断、同一点の大量重複を検証する。

ローカル実データでも取り込みと未編集E57の書き戻しを照合した。

| データ | スキャン | 点数 | 画像 | 確認 |
| --- | ---: | ---: | ---: | --- |
| manitouNoInvalidPoints.e57 | 5 | 1,095,702 | 5 | 元の点属性、スキャン・画像pose、投影・画像データ |
| Trimble_StSulpice-Cloud-50mm.e57 | 5 | 8,484,455 | 0 | RGBを含む元の点属性、スキャンpose、DirectX 12表示 |

この規模での確認は、億点以上の性能検証を置き換えるものではない。次の段階で大規模データの処理時間・RAM・一時ディスク使用量を計測する。

Zstd導入後の実データの資産容量（点・表示用LOD・画像・元XML、manifestを除く。表示用8分木の導入前）は、manitouが104.1 MB→34.4 MB、Trimbleが684.5 MB→220.4 MB。既存プロジェクトのコピーを変換して検証した。

取り込みの並列化後、Trimbleの848万点を同じPCのRelease版で計測した。変更前は約12.6秒、変更後は1ワーカーで約9.5秒、8ワーカーで約7.2〜7.6秒。並列化に加えてLOD候補の全点ソートを省いた改善を含む。ローカルディスク／OSキャッシュの影響がある実測値で、ストレージや点属性によって変わる。変更前のプロジェクトと全原解像度チャンク・点参照・木構造・全LODを照合した。画像入りmanitouでも同じ照合を行った。

点群処理と書き出しも同じTrimble（848万点）のRelease版で計測した。5cmのボクセル間引きが約1.0秒、続けて半径0.1m・4点未満のノイズ除去が約1.6秒、SOR（6点・1σ、間引き前）が約3.2秒（どちらもスキャンごと・最大16スレッド）。全点のLAS書き出しが約2.7秒（221 MB）、LAZが約4.6秒（104 MB）。

ICPは`align_bench`で、各スキャンに既知のずれ（スキャン中心周りに約1.8°と15cm）を与えて他のスキャンに合わせ直して確かめられる。manitou（5スキャン、E57から取り込み直したもの）では開始距離0.5m・終了距離0.02mで、重なり45〜69%の4スキャンがスキャン範囲の隅で0.5〜1.1cm以内に戻り、1スキャン約0.6秒（0.5mの1段だけでは10〜20cm）。重なり11%のスキャンは合わない（2026-10-05）。ずれなしから始めてもICPの解は元の位置合わせから1〜4cm動くため、これが比較の下限になる。重なりが5%程度のスキャンは合わない。

```powershell
cargo run -p geemil-core --release --example align_bench -- work-data/manitou 0.5 1
# 終了距離は6番目の引数（既定0.02）。開始距離と同じにすると1段だけで合わせる
cargo run -p geemil-core --release --example align_bench -- work-data/manitou 0.5 1 60000 50 0.5
```

全体の最適化は`global_bench`で、最初のスキャン以外に、後のスキャンほど大きいずれ（1スキャンごとに約0.1°と1cmずつ増える）を与えて、最初のスキャンを固定して合わせ直して確かめられる。プロジェクトは一時フォルダーへ複製して扱い、元は変更しない。manitouでは、ずれを与えた場合も与えない場合も同じ解（差は約1cm以内）に収まり、組ごとのRMSの平均は6.8mmから5.6mmに下がった（約5秒）。実データには正解がないため、隅の誤差は元の位置合わせとの差を表す。

```powershell
cargo run -p geemil-core --release --example global_bench -- work-data/manitou 1
```

## 1億点での実測

現形式のレイヤー、LAS属性保持、メモリ予算付きフィルターでの再検証は[2026-10-04の性能検証](performance-review-20261004.md)を参照。現版のReleaseビルドで1億点を取り込み直し、間引き・ノイズ除去・Undo/Redo・1,200視点のCPU表示更新・LAS/LAZ書き出しまで通した。

以下は、点のレイヤーを導入する前（除外レイヤーをビットマスクで持っていた版）の過去の計測値。現在の性能値やメモリ上限としては扱わない。

`make_synthetic`で地上レーザー風の合成データ（10スキャン×1,000万点のLAS、計2.5 GB。スキャナー位置から角度一様に光線を出し、地面・円形の壁・箱に当たった点。近いほど密）を作り、同じPC（Core i5-12400F、RAM 64 GB、RTX 3060、Release版）で測った。メモリは各プロセスのピークのワーキングセット。

| 処理 | 時間 | ピークメモリ |
| --- | ---: | ---: |
| 取り込み（10ファイル、1億点） | 186秒 | 78 MB |
| 表示（200万点の読み込み、起動から） | 0.19秒 | 414 MB（GUI全体） |
| ボクセル間引き 2 cm（7,042万点を移動） | 28秒 | 493 MB |
| ノイズ除去 半径5 cm・4点未満（609万点を移動） | 26秒 | 271 MB |
| LAS書き出し（残り2,349万点、0.57 GB） | 35秒 | 23 MB |
| LAZ書き出し（同、0.25 GB） | 39秒 | 26 MB |
| ICP（1スキャン、6万サンプル） | 約0.25秒 | 19 MB |

- プロジェクトのディスク使用量は3.1 GB（元のLASは2.5 GB）。取り込み中は1ファイル分の展開データ（1,000万点で約0.9 GB）を一時的に`staging`へ置く。
- 当時の表示のGPUメモリは、点の頂点バッファー（200万点×28バイト＝約56 MB）と画面サイズの描画先が中心。現版はノード単位の常駐バッファーとキャッシュを使う。
- メモリは点数ではなくチャンク・キャッシュの予算で決まり、1億点でも数百MBに収まった。処理時間はほぼ点数に比例する。
- この合成シーンは円形の壁と規則的な箱の並びで回転方向の手がかりが少ないため、ICPの精度評価には向かない（ずれなしから始めても隅で最大1 m動く）。精度は実データのmanitouで確認している。

```powershell
cargo run -p geemil-core --release --example make_synthetic -- work-data/syn100m 10 10000000
cargo run -p geemil-core --release -- import work-data/syn100m-p (Get-ChildItem work-data/syn100m/*.las)
```

工程表示ごとの経過時間と取り込み全体を測るには、未作成の出力フォルダーを指定する。第3引数はワーカー数（0は自動）、第4引数は変換バッファーの予算MiB、第5引数は任意の比較用プロジェクト。工程の処理は重なるため、表示された時間は各工程の単独CPU時間ではない。

```powershell
cargo run -p geemil-core --release --example import_bench -- Trimble_StSulpice-Cloud-50mm.e57 work-data/bench-8 8 256
```

CPU側の読み込みは、`view_bench`で同じ周辺を少しずつ回転して計測できる。GPU転送・描画は含まない。Trimbleの約20万点では、導入前のDebug版が毎回約122〜131ms、キャッシュ導入後のDebug版は再利用時約7〜9ms。圧縮後のRelease版は初回約21ms、再利用時約1.5〜1.8msだった。ディスク読み込み・展開・座標変換の初回コストは残る。DebugとReleaseの数字を同じ条件の高速化率として比較しない。

表示用ノードの読み込み・解凍・点への復元は、CPU数−1（最大8）のワーカーで並列化する。処理中・完了待ちのノードと展開用の一時メモリには256 MiBの上限を設け、キャッシュ済みノードを先に表示する。全レイヤー表示時は点ごとの非表示判定を省く。`view_bench PROJECT BUDGET WORKERS`のWORKERSは0で自動、1で直列。ノード選択と読み込み時間を別々に出力する。Manitouの326ノード・約110万点では、Release版の初回CPU読み込みが変更前の約143msから自動ワーカーで約24〜34msになった（ディスクキャッシュの影響を含む）。GUI自動テストはCPU結果受信までと全GPU転送の投入までを記録し、GPU処理完了・画面への提示時間とは区別する。

```powershell
cargo run -p geemil-core --release --example view_bench -- work-data/trimble
```

点数ではなくスキャン数・編集回数で増える処理は`scale_bench`で計測できる。初回は指定したLASフォルダーからプロジェクトを作り、スキャンをフォルダーに分け、指定回数の小さな範囲選択の移動でラベルのパッチを作る（2回目以降は作成済みのプロジェクトを使う）。スキャン一覧、ツリーのフォルダー集計、表示ノードの選択（キャッシュなし・あり）とノード読み込みの時間を出力する。300スキャン（各3万点）・20フォルダー・パッチ199件で、表示更新ごとのノード選択は約3.4msから約2.7ms、キャッシュ済みノードの読み込みは約9〜10msから約0.4〜1.5msになった。各点群チャンクのレイヤー別点数はプロジェクトの状態ごとに一度だけ求めるため、チャンクの多い大規模データほど差が大きくなる。

```powershell
cargo run -p geemil-core --release --example make_synthetic -- work-data/syn-scale-las 300 30000
cargo run -p geemil-core --release --example scale_bench -- work-data/syn-scale-proj work-data/syn-scale-las 20 200
```

選択範囲の移動は`selection_bench`で計測できる。初期の全体表示で画面中央の正方形（第2引数は半幅、0.5で画面全体）を奥行き（第3引数、m）付きで「Deleted」レイヤーへ移す。プロジェクトを書き換えるため、コピーに対して実行する。原解像度チャンクのうち選択範囲・奥行きに入り得ないものは読み込まない。以下はレイヤー導入前の計測値。Trimbleの848万点のRelease版で、チャンクを読み飛ばす変更の前は範囲によらず約2.4秒、変更後は中央の小範囲で約0.5秒、画面全体・奥行き0.5mで約0.75秒、全点で約1.2秒。結果は変更前とbit単位で一致した。

```powershell
cargo run -p geemil-core --release --example selection_bench -- work-data/trimble-copy 0.05 0.5
```

第3引数を`inf`にすると奥行きを制限しない。第4引数に`outside`を指定すると範囲外（切り出し）を計測する。同じ848万点で中央の正方形（半幅0.15）の外側26.7万点は約0.8秒、内側821.7万点の奥行き無制限は約0.85秒で、両者の合計は全点数と一致した。

開発用のGPU起動テストでは、2秒間カメラを回転・平行移動し、移動中にもノードの更新が完了することを確認して画面を保存する。

```powershell
cargo run -p geemil-desktop --release -- work-data/trimble --smoke-test work-data/preview.png --smoke-orbit
```

RGB表示の検証では、既知の12色をGPUとeguiを経由して表示し、スクリーンショットのpixel値を入力と照合する。

```powershell
cargo run -p geemil-desktop --release -- --smoke-test work-data/colors.png --smoke-colors
```

移動プレビューの確認では、起動1秒後に画面中央を矩形選択し、赤表示の点数と基準点を出力して画面を保存する。`inside`は奥行き0.5mの範囲内、`inside-all`は奥行き無制限の範囲内、`outside`は範囲外。

```powershell
cargo run -p geemil-desktop --release -- work-data/trimble --smoke-test work-data/select.png --smoke-select outside
```

画面の確認用に、ダイアログを開いた状態（`revisions`・`shortcuts`・`new-project`・`cleanup`・`save-as`）を保存できる。プロジェクトを指定しないと起動画面を保存する。

```powershell
cargo run -p geemil-desktop --release -- work-data/manitou --smoke-test work-data/revisions.png --smoke-dialog revisions
```

`--smoke-script`は、表示の読み込み後に操作を順に実行し、各操作後のスキャン数・フォルダー数・レイヤーごとの点数・リビジョン数・未保存の有無・元に戻す／やり直すの可否を出力する。操作は`exclude`（選択した点を「削除」へ移す）・`undo`・`redo`・`save`・`folder`・`remove`・`measure`・`preview`（最初のスキャンの変換を入力するが適用しない。動いた表示点数を出力する）・`apply-transform`（入力中の変換を適用する）・`subsample`（5cmのボクセル間引き）・`noise`（半径0.1m・4点未満のノイズ除去）・`sor`（6点・1σの統計的外れ値除去）・`subsample-merged`（表示中のスキャンをまとめて5cmで間引き）・`align-icp`（最初のスキャンを他のスキャンにICPで合わせてプレビュー）・`align-pairs`（一致する4組の対応点で合わせ、動かないことを確認）・`align-global`（最初のスキャンを固定して全体を最適化）・`align-list`（位置合わせの一覧を開き、済み・要確認・未実施の数と全体最適化の前後のRMSを出力）・`align-switch`（最後のスキャンを選び、未適用の結果の確認が出たかを出力）・`align-leave`（「カメラ操作」ツールへの切り替えを試み、未適用の結果の確認が出たかを出力）・`align-apply`（プレビュー中の結果を適用）・`ortho`（平行投影の上面図）・`box`（表示点の中央の高さに2mの水平スライスを置き内部をハイライト、箱の中の表示点数を出力）・`box-resize`（傾けた箱でサイズ変更の取っ手を表示）・`box-crop`（その箱の外を「削除」へ移す）・`transform`／`transform-folder`（最初のスキャンかフォルダーを選び「移動・回転」ツールにする）・`place-handles`（画面中央をダブルクリックしたものとして取っ手を置き、前後の位置を出力）・`show-layers`（すべてのレイヤーを表示）・`restore`（最も新しいレイヤーの点を全部「点群」へ戻す）・`color-height`／`color-scan`（色の表示方法）・`adaptive`（点サイズを適応にする）・`focus`（最初のスキャンにカメラを寄せる）・`zoom`（カメラ距離を1/8にする）。各操作の行は操作を始めた時点の状態で、バックグラウンド処理（`job true`）の結果は次の行に出る。`--smoke-dialog`には`subsample`と`noise`も指定できる。`--smoke-budget N`で描画点数を変えられる（例: `--smoke-orbit --smoke-budget 20000000`）。プロジェクトを書き換えるため、コピーに対して実行する。

```powershell
cargo run -p geemil-desktop --release -- work-data/manitou-copy --smoke-test work-data/flow.png --smoke-select inside-all --smoke-script exclude,undo,redo,folder,save,remove,measure
```

Potreeのカメラ・ノード表示処理の参照結果と色修正の詳細は[docs/potree-rendering-notes.md](potree-rendering-notes.md)にまとめている。

## 現形式の連続処理・長時間表示の検証

`workflow_bench`は新しいプロジェクトを作り、取り込み、2cm間引き、Undo/Redoを20往復、半径5cm・4点未満のノイズ除去、指定数の視点のCPU表示更新、LAS/LAZ書き出しを連続して実行する。入力と既存プロジェクトは変更しない。出力にはWindowsのプロセスメモリ・staging容量の50msごとの観測ピークと、CPU表示更新のp50/p95/p99を含む。短い工程ではサンプリングがピークを見逃すため、プロセス起動以降のOS累積ピークも記録する。GPU描画や画面提示の時間は含まない。

```powershell
cargo run -p geemil-core --release --example workflow_bench -- work-data/bench-current work-data/syn100m 1200
```

GUIの長時間検証はPowerShell 7で行う。`--smoke-orbit-seconds`は回転・平行移動の時間を指定し、対象から離れない経路を使う。`scripts/measure-gui.ps1`はRAMとWindowsのGPUプロセスメモリカウンターを約500ms間隔で記録し、PNG・ログ・metrics JSONを保存する。GPUカウンターが取得できなければ値はnullとなる。フレーム間隔はUI callbackの観測値で、GPU fenceや画面提示を直接測定した値ではない。既存の出力PNGは上書きしない。`-SmokeScript`で編集を実行する場合はコピーしたプロジェクトに対して使う。

```powershell
cargo build -p geemil-desktop --release
.\scripts\measure-gui.ps1 -Project work-data/bench-current -OutputPrefix work-data/bench-gui -OrbitSeconds 120
```

ローカルの入力E57、作業プロジェクト、ビルド成果物はリポジトリに含めない。
