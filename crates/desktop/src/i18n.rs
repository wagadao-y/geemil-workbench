//! User-facing text. The core reports English labels and typed stages/errors;
//! each language is one `Strings` table, so a missing entry fails to compile.
use geemil_core::{CoreError, Stage};

pub struct Strings {
    pub thousands_separator: char,

    pub new_project: &'static str,
    pub new_project_dialog: &'static str,
    pub open: &'static str,
    pub open_dialog: &'static str,
    pub import: &'static str,
    pub point_cloud_filter: &'static str,
    pub export_e57: &'static str,
    pub compress_storage: &'static str,
    pub fit_view: &'static str,

    pub scans: &'static str,
    pub no_project_hint: &'static str,
    pub scan_summary: fn(points: &str, chunks: &str) -> String,
    pub images: &'static str,
    pub unnamed_image: &'static str,
    pub omitted_attributes: &'static str,
    pub alignment: &'static str,
    pub translation: &'static str,
    pub rotation: &'static str,
    pub apply_transform: &'static str,
    pub layers: &'static str,
    pub manual_exclusion_layer: fn(points: &str) -> String,
    pub history: &'static str,
    pub default_branch_name: &'static str,
    pub fork: &'static str,
    pub fork_hint: &'static str,

    pub revision_created: &'static str,
    pub revision_import: fn(file: &str) -> String,
    pub revision_exclude: fn(points: &str) -> String,
    pub revision_layer: fn(layer: &str, enabled: bool) -> String,
    pub revision_transform: fn(scan: &str) -> String,

    pub navigate: &'static str,
    pub select: &'static str,
    pub polygon: &'static str,
    pub depth: &'static str,
    pub exclude_selection: &'static str,
    pub clear_selection: &'static str,
    pub point_budget: &'static str,
    pub point_size: &'static str,
    pub controls_hint: &'static str,

    pub status_start: &'static str,
    pub status_saved: &'static str,
    pub status_working: &'static str,
    pub status_done: &'static str,
    pub status_failed: &'static str,
    pub status_cancelled: &'static str,
    pub cancel: &'static str,
    pub view_stats: fn(points: &str, ms: f64) -> String,
    pub dismiss: &'static str,
    pub details: &'static str,
    pub job_panicked: &'static str,
    pub unexpected_error: &'static str,

    pub stage: fn(Stage) -> &'static str,
    pub core_error: fn(&CoreError) -> String,
}

impl Strings {
    /// Point and chunk counts with digit grouping, e.g. 8,475,123.
    pub fn count(&self, n: impl Into<u64>) -> String {
        let digits = n.into().to_string();
        let mut out = String::with_capacity(digits.len() + digits.len() / 3);
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i) % 3 == 0 {
                out.push(self.thousands_separator);
            }
            out.push(c);
        }
        out
    }
    /// Displayed point totals in megapoints, e.g. 0.20 MP.
    pub fn mega_points(&self, n: usize) -> String {
        format!("{:.2} MP", n as f64 / 1e6)
    }
}

pub static JA: Strings = Strings {
    thousands_separator: ',',

    new_project: "新規プロジェクト",
    new_project_dialog: "新しいプロジェクトの保存先（新規フォルダー名）",
    open: "開く",
    open_dialog: "プロジェクトフォルダーを開く",
    import: "点群を取り込む",
    point_cloud_filter: "点群",
    export_e57: "E57書き出し",
    compress_storage: "内部データを圧縮",
    fit_view: "全体表示",

    scans: "スキャン",
    no_project_hint: "元ファイルを移動しても使える作業プロジェクトを作成します。",
    scan_summary: |points, chunks| format!("{points} 点 / {chunks} チャンク"),
    images: "対応画像",
    unnamed_image: "画像",
    omitted_attributes: "取り込み時に省略した属性",
    alignment: "位置合わせ（追加変換）",
    translation: "平行移動（元データの座標単位）",
    rotation: "回転（度）",
    apply_transform: "変換を確定",
    layers: "除外レイヤー",
    manual_exclusion_layer: |points| format!("手動除外（{points} 点）"),
    history: "履歴",
    default_branch_name: "新しい分岐",
    fork: "現在の状態から分岐",
    fork_hint: "切り替え後の編集も、自動的に別の枝になります。",

    revision_created: "プロジェクト作成",
    revision_import: |file| format!("取り込み: {file}"),
    revision_exclude: |points| format!("除外: {points} 点"),
    revision_layer: |layer, enabled| {
        if enabled {
            format!("レイヤーを有効化: {layer}")
        } else {
            format!("レイヤーを無効化: {layer}")
        }
    },
    revision_transform: |scan| format!("手動変換: {scan}"),

    navigate: "カメラ操作",
    select: "範囲選択",
    polygon: "多角形",
    depth: "奥行き",
    exclude_selection: "選択範囲を除外",
    clear_selection: "選択解除",
    point_budget: "描画点数上限",
    point_size: "点サイズ",
    controls_hint: "左ドラッグ: 回転 / 矩形選択    右・中ドラッグ: 平行移動    ホイール: 拡大縮小    多角形: 左クリックで頂点を追加",

    status_start: "プロジェクトを作成するか、既存のプロジェクトを開いてください。",
    status_saved: "プロジェクトは保存済みです。",
    status_working: "処理中…",
    status_done: "完了しました。プロジェクトは保存済みです。",
    status_failed: "処理を終了しました。",
    status_cancelled: "キャンセルしました。完了済みの処理は保存されています。",
    cancel: "キャンセル",
    view_stats: |points, ms| format!("表示 {points} / 更新 {ms:.1} ms"),
    dismiss: "閉じる",
    details: "詳細",
    job_panicked: "処理スレッドで予期しないエラーが発生しました。直前の確定状態は保持されています。",
    unexpected_error: "処理中にエラーが発生しました。",

    stage: |stage| match stage {
        Stage::ReadingE57 => "E57を読み込み中",
        Stage::ReadingLas => "LAS/LAZを読み込み中",
        Stage::Images => "画像を取り込み中",
        Stage::Partitioning => "空間分割",
        Stage::Indexing => "インデックス構築",
        Stage::BuildingParentLod => "LODを構築中",
        Stage::CompressingPoints => "点データを圧縮中",
        Stage::CompressingLod => "LODを圧縮中",
        Stage::VerifyingCompressedPoints => "圧縮データを検証中",
        Stage::SelectionNearestDepth => "選択範囲の手前の点を検索中",
        Stage::SelectionExclusionMask => "除外範囲を作成中",
        Stage::WritingE57 => "E57を書き出し中",
        Stage::ViewLod => "表示LODを選択中",
        Stage::ViewPoints => "表示点を読み込み中",
    },
    core_error: |error| match error {
        CoreError::Cancelled => "キャンセルしました。".into(),
        CoreError::UnsupportedFormat => "対応している形式はE57、LAS、LAZです。".into(),
        CoreError::OutputExists(p) => {
            format!("書き出し先のファイルが既にあります: {}", p.display())
        }
        CoreError::ProjectExists(p) => {
            format!(
                "このフォルダーは既に存在します。新しいフォルダー名を指定してください: {}",
                p.display()
            )
        }
        CoreError::NotAProject(p) => {
            format!(
                "プロジェクトフォルダーではありません（project.jsonがありません）: {}",
                p.display()
            )
        }
        CoreError::UnsupportedProjectFormat(v) => {
            format!("このバージョンでは開けないプロジェクト形式です（形式バージョン {v}）。")
        }
        CoreError::CoordinateSystemMismatch => {
            "座標系の異なるスキャンを一つのE57へ書き出すことはできません。".into()
        }
    },
};

#[cfg(test)]
mod tests {
    use super::JA;

    #[test]
    fn counts_are_grouped_and_megapoints_rounded() {
        assert_eq!(JA.count(0u64), "0");
        assert_eq!(JA.count(999u64), "999");
        assert_eq!(JA.count(1_000u64), "1,000");
        assert_eq!(JA.count(8_475_123u64), "8,475,123");
        assert_eq!(JA.count(u64::MAX), "18,446,744,073,709,551,615");
        assert_eq!(JA.mega_points(200_000), "0.20 MP");
        assert_eq!(JA.mega_points(2_000_000), "2.00 MP");
    }
}
