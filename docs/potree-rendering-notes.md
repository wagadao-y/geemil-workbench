# Potreeの表示実装を参照したメモ

対象はローカルの`ref/potree`。今回の操作・色修正と、今後の大規模表示で参考になる部分を記録する。

## カメラ操作

`src/navigation/OrbitControls.js`は左ドラッグをyaw/pitchの変更、右ドラッグをパンに割り当てる。回転時はpivotを保ったままカメラ位置を更新し、パン距離はview radiusと画面上の移動割合で調整している。

Workbenchはカメラ操作方式を作業ツールから独立させ、中央ドラッグは回転、右ドラッグはパンに割り当てる。矩形・多角形選択と編集用の取っ手を除き、左ドラッグでも回転できる。中央クリックはスキャン選択、中央ダブルクリックは注視点の変更。オービットでは注視点の周りを回り、フライ・ウォークではカメラ位置を保って見回す。Cキーで操作方式を循環し、WASD・Q/Eはどの作業ツールでも移動に使い、ウォークのWASDは水平移動に限定する。

`ref/potree/src/navigation/OrbitControls.js`は更新時に`view.radius`を移動速度へ設定し、パンも半径に比例させる。WorkbenchもオービットのWASD・Q/Eを注視点までの距離に比例させる。フライ・ウォークの設定速度は別に保持する。

Potreeの減衰は導入していない。Workbenchのダブルクリックではピック位置へ注視点を移す。手動除去では描き終えた選択とそのカメラを保持し、移動しても選択条件が変わらないようにする。

## ノード選択・読み込み

`src/Potree_update_visibility.js`で参考になる点:

- frustumとカメラ位置をスキャンのローカル座標系で計算する。
- 透視投影時のノードの画面上の半径を優先度とし、最小pixel size以下のノードを展開しない。
- 全体と点群ごとの点数予算を持つ。
- 読み込めたノードをGPU表示用へ移す数を1フレームあたり制限する。参照したコードでは2ノード。
- 表示ノードをLRUへtouchし、非同期の読み込み数にも上限を設ける。

`src/LRU.js`は連結リストとnode IDの表で参照順を更新する。現在のWorkbenchには優先度キューとRAMのノードキャッシュがあるが、カメラ位置に応じた画面pixel sizeの判定、ノード単位のGPU常駐と転送予算は今後の改善候補。Potreeの上限値をそのまま採用するのではなく、点数・転送byte数・処理時間を計測して決める。

Potreeのノードには段階的な代表点が含まれる。一方、Workbenchは原解像度leafと別管理のLODを持つため、親子を同時に描く場合の重複の扱いは別途設計が必要。

## RGBの表示経路

Potreeの`src/materials/shaders/pointcloud.vs`の`getRGB()`はRGBにgamma・brightness・contrastの調整を適用する。参照した`PointCloudMaterial.js`の既定値は`[1, 0, 0]`。この調整パラメーターと、フレームバッファーのsRGB変換は別の処理として考える。

Workbenchで白っぽく見えた直接の原因は、egui-wgpuへのテクスチャ受け渡しの不整合だった。egui-wgpu 0.31.1の`register_native_texture`は`Rgba8UnormSrgb`を前提とし、`egui.wgsl`はサンプル値をリニアRGBとしてsRGBに変換する。旧実装は`Rgba8Unorm`に入力のsRGB値をそのまま保存していたため、eguiで余分な明るさ変換が入っていた。

egui-wgpu 0.36では前提が逆になり、`register_native_texture`は`Rgba8Unorm`にsRGB符号値を保持したテクスチャを前提とする（`egui.wgsl`は「sRGB非対応の通常のテクスチャ」としてサンプルする）。0.31向けの経路のまま更新すると、暗部が二重にリニア化されて沈んだ（入力8が1として表示）。`--smoke-colors`で検出した。

現在の経路:

```text
入力の8bit sRGB
  → 点シェーダーでsRGBからリニアRGBへ変換
  → Rgba8UnormSrgbの描画テクスチャへ描画（GPUがsRGBにエンコード）
  → EDLパスがtextureLoadで読む（GPUがリニアRGBへデコード）、陰影を掛ける
  → EDLシェーダーでリニアRGBからsRGB符号値へ変換し、Rgba8Unormの出力テクスチャへ書く
  → eguiが出力テクスチャをsRGB符号値としてサンプルし、画面出力処理を行う
```

alphaはRGBと別に扱い、sRGB変換しない。背景のclear値はリニアRGB。これは表示処理の修正で、既存プロジェクトのRGBにも適用される。

`--smoke-colors`は既知の12色の点を描画し、ネイティブ画面のスクリーンショットの中心pixelを入力値と照合する。黒・白・暗部・sRGBの区分境界付近・中間灰色・混合色を含む。許容差は各チャンネル2コード値で、検証環境ではすべて完全一致した。中間灰色128も128として表示された。

CloudCompareとの画面比較は、RGB以外にも点サイズ・ライティング・シェーダー設定等で見え方が変わる。この検証はWorkbenchのRGB出力経路が入力のsRGBコード値を保持することを確認するもの。
