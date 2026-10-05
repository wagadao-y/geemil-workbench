# Geemil Workbench

Windows向けのOSS点群編集アプリ。Rust / egui / wgpu製で、億点規模のレーザースキャンデータを、全点をメモリに載せずに表示・編集できる。

現在は初期プロトタイプ。プロジェクト形式は暫定版で、将来の版との互換性は保証しない。

## 起動

WindowsのRust MSVC環境とC++ Build Toolsが必要（Rustの版は`rust-toolchain.toml`で固定しており、rustupが自動で取得する）。

```powershell
cargo run -p geemil-desktop --release
cargo run -p geemil-desktop --release -- work-data/trimble   # 既存のプロジェクトを開く
```

配布物は`cargo build -p geemil-desktop --release`で作る`target/release/geemil-desktop.exe`の1ファイルだけ。

## できること

- **取り込み**: E57・LAS・LAZ。元の属性・pose・画像を保持する
- **表示**: Potree方式のLOD表示、RGB／高さ／スキャンごとの色分け、平行投影、EDL
- **選択と編集**: 矩形・多角形・3Dボックスで点を選び、レイヤーへ移す（点は消さない）
- **フィルター**: ボクセル間引き、ノイズ除去、統計的外れ値除去（SOR）、スキャンの重なりの整理、動体除去
- **位置合わせ**: 手動の移動・回転、対応点、ICP、全スキャンの同時最適化
- **履歴**: 元に戻す／やり直す、リビジョンの保存と分岐
- **書き出し**: E57、LAS/LAZ
- **計測**: 2点間の距離

キー操作の一覧はアプリの「ヘルプ → キーボードショートカット」にある。

## 主な制約

- 座標単位はメートル前提。座標系の変換はしない
- 統合点群の作成、画像からの再着色、LAS/LAZの波形パケットは未対応

## ドキュメント

- [使い方](docs/user-guide.md): 操作・対応形式・制約の詳細
- [開発・検証](docs/development.md): CLI、テスト、ベンチマークと実測値
- [設計方針](docs/design.md) / [内部形式](docs/internal-format.md)

## ライセンス

Apache-2.0
