//! User-facing text. The core reports English labels and typed stages/errors;
//! each language is one `Strings` table, so a missing entry fails to compile.
use geemil_core::{CleanupReport, CoreError, Stage};

pub struct Strings {
    pub thousands_separator: char,

    // Menus.
    pub menu_file: &'static str,
    pub menu_edit: &'static str,
    pub menu_view: &'static str,
    pub menu_tools: &'static str,
    pub menu_help: &'static str,

    // Actions, as shown in menus, toolbar tooltips and the shortcut list.
    pub new_project: &'static str,
    pub open: &'static str,
    pub recent: &'static str,
    pub recent_empty: &'static str,
    pub import: &'static str,
    pub export: &'static str,
    pub quit: &'static str,
    pub undo: &'static str,
    pub redo: &'static str,
    pub clear_selection: &'static str,
    pub exclude_selection: &'static str,
    pub new_folder: &'static str,
    pub fit_view: &'static str,
    pub view_top: &'static str,
    pub view_front: &'static str,
    pub view_side: &'static str,
    pub view_iso: &'static str,
    pub edl: &'static str,
    pub ortho: &'static str,
    pub color_by: &'static str,
    pub color_rgb: &'static str,
    pub color_height: &'static str,
    pub color_scan: &'static str,
    pub save: &'static str,
    pub revisions: &'static str,
    pub discard: &'static str,
    pub cleanup: &'static str,
    pub export_format: &'static str,
    pub export_message: &'static str,
    pub export_e57_message: &'static str,
    pub export_las_to_e57_notice: &'static str,
    pub filter_memory: &'static str,
    pub filter_memory_hint: &'static str,
    pub export_las_message: &'static str,
    pub export_preserve_attributes: &'static str,
    pub export_omit_incompatible: &'static str,
    pub export_conflicts: &'static str,
    pub export_omit_extra: &'static str,
    pub export_omit_gps: &'static str,
    pub export_omit_crs: &'static str,
    pub export_no_conflicts: &'static str,
    pub export_merged: &'static str,
    pub export_per_scan: &'static str,
    pub export_button: &'static str,
    pub subsample: &'static str,
    pub remove_noise: &'static str,
    pub remove_outliers: &'static str,
    pub outliers_message: &'static str,
    pub outlier_neighbours: &'static str,
    pub outlier_deviations: &'static str,
    pub outlier_reach: &'static str,
    pub subsample_message: &'static str,
    pub noise_message: &'static str,
    pub voxel_size: &'static str,
    pub search_radius: &'static str,
    pub min_neighbours: &'static str,
    pub filter_targets: fn(scans: usize) -> String,
    pub run: &'static str,
    pub points_moved: fn(points: &str, layer: &str) -> String,
    pub status_no_change: &'static str,
    pub shortcuts: &'static str,
    pub about: &'static str,

    // Tools.
    pub navigate: &'static str,
    pub tool_rect: &'static str,
    pub tool_polygon: &'static str,
    pub tool_measure: &'static str,
    pub tool_align: &'static str,
    pub tool_box: &'static str,
    pub tool_transform: &'static str,
    pub hint_transform: &'static str,
    pub hint_box: &'static str,
    pub box_title: &'static str,
    pub box_hint: &'static str,
    pub box_to_view: &'static str,
    pub box_to_view_hint: &'static str,
    pub box_reset: &'static str,
    pub box_highlight: &'static str,
    pub box_exclude_outside: &'static str,
    pub box_exclude_inside: &'static str,
    pub hint_align: &'static str,
    pub align_title: &'static str,
    pub align_choose_item: &'static str,
    pub align_moving: &'static str,
    pub align_reference: &'static str,
    pub align_reference_scans: fn(scans: usize) -> String,
    pub align_tint: &'static str,
    pub align_show: &'static str,
    pub align_show_both: &'static str,
    pub align_pairs: &'static str,
    pub align_pairs_hint: &'static str,
    pub align_residual: &'static str,
    pub remove: &'static str,
    pub align_fit_pairs: &'static str,
    pub align_need_three: &'static str,
    pub align_clear_pairs: &'static str,
    pub align_icp: &'static str,
    pub align_icp_hint: &'static str,
    pub align_icp_distance: &'static str,
    pub align_icp_samples: &'static str,
    pub align_run_icp: &'static str,
    pub align_pairs_result: fn(rms: f64) -> String,
    pub align_icp_result: fn(rms: f64, overlap: f64, iterations: u32) -> String,
    pub align_low_overlap: &'static str,
    pub align_discard: &'static str,
    pub hint_navigate: &'static str,
    pub hint_rect: &'static str,
    pub hint_polygon: &'static str,
    pub hint_measure: &'static str,
    pub exclude_inside: &'static str,
    pub exclude_outside: &'static str,
    pub depth: &'static str,
    pub preview_count: fn(points: &str) -> String,
    pub preview_searching: &'static str,
    pub measure_result: fn(distance: f64, horizontal: f64, vertical: f64) -> String,

    // Display settings.
    pub point_budget: &'static str,
    pub point_size: &'static str,
    pub edl_strength: &'static str,

    // Project tree and properties.
    pub no_project_hint: &'static str,
    pub scan_points: fn(points: &str) -> String,
    pub folder_summary: fn(scans: usize, points: &str) -> String,
    pub rename: &'static str,
    pub move_to: &'static str,
    pub top_level: &'static str,
    pub ungroup: &'static str,
    pub remove_scans: &'static str,
    pub show_only: &'static str,
    pub show: &'static str,
    pub hide: &'static str,
    pub show_all: &'static str,
    pub drop_to_top: &'static str,
    pub default_folder_name: &'static str,
    pub properties: &'static str,
    pub tree_selected: fn(usize) -> String,
    pub nothing_selected: &'static str,
    pub prop_name: &'static str,
    pub prop_points: &'static str,
    pub prop_valid: &'static str,
    pub prop_chunks: &'static str,
    pub prop_source: &'static str,
    pub prop_scans: &'static str,
    pub images: &'static str,
    pub unnamed_image: &'static str,
    pub omitted_attributes: &'static str,
    pub transform: &'static str,
    pub transform_folder_hint: &'static str,
    pub translation: &'static str,
    pub rotation: &'static str,
    pub apply: &'static str,
    pub reset: &'static str,
    pub transform_previewing: &'static str,
    pub revert: &'static str,
    pub subsample_merged: &'static str,
    pub subsample_merged_hint: &'static str,

    // Layers.
    pub layers: &'static str,
    pub layers_hint: &'static str,
    pub default_layer: &'static str,
    pub layer_deleted: &'static str,
    pub layer_noise: &'static str,
    pub layer_subsampled: &'static str,
    pub new_layer: &'static str,
    pub new_layer_title: &'static str,
    pub new_layer_suffix: fn(name: &str) -> String,
    pub destination: &'static str,
    pub move_all_points: &'static str,
    pub delete_layer: &'static str,

    // Revisions.
    pub revisions_title: &'static str,
    pub revisions_lead: &'static str,
    pub unsaved_state: &'static str,
    pub open_revision: &'static str,
    pub delete_revision: &'static str,
    pub shown_revision: &'static str,
    pub close: &'static str,
    pub default_revision_name: fn(n: usize) -> String,
    pub revision_created: &'static str,
    pub operation_kind: fn(kind: &str) -> &'static str,
    pub save_title: &'static str,
    pub name: &'static str,
    pub save_button: &'static str,
    pub cancel: &'static str,
    pub discard_title: &'static str,
    pub discard_message: &'static str,
    pub discard_and_continue: &'static str,
    pub cleanup_title: &'static str,
    pub cleanup_message: &'static str,
    pub cleanup_run: &'static str,
    pub cleanup_done: fn(report: &CleanupReport, size: &str) -> String,

    // New project and welcome screen.
    pub new_project_title: &'static str,
    pub project_name: &'static str,
    pub location: &'static str,
    pub browse: &'static str,
    pub create: &'static str,
    pub folder_will_be: fn(path: &str) -> String,
    pub import_after: fn(files: usize) -> String,
    pub welcome_lead: &'static str,
    pub recent_projects: &'static str,
    pub drop_hint: &'static str,
    pub dropped_without_project: &'static str,
    pub empty_project: &'static str,

    // Status bar and messages.
    pub status_start: &'static str,
    pub status_opened: &'static str,
    pub status_working: &'static str,
    pub status_done: &'static str,
    pub status_failed: &'static str,
    pub status_cancelled: &'static str,
    pub status_saved: fn(name: &str) -> String,
    pub status_unsaved: &'static str,
    pub status_revision: fn(name: &str) -> String,
    pub view_stats: fn(points: &str, ms: f64) -> String,
    pub dismiss: &'static str,
    pub details: &'static str,
    pub job_panicked: &'static str,
    pub unexpected_error: &'static str,
    pub shortcut_rows: &'static [(&'static str, &'static str)],
    pub about_text: &'static str,

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
    /// Byte sizes with binary units, e.g. 1.5 GiB.
    pub fn bytes(&self, n: u64) -> String {
        let units = ["B", "KiB", "MiB", "GiB", "TiB"];
        let mut value = n as f64;
        let mut unit = 0;
        while value >= 1024. && unit + 1 < units.len() {
            value /= 1024.;
            unit += 1;
        }
        if unit == 0 {
            format!("{n} B")
        } else {
            format!("{value:.1} {}", units[unit])
        }
    }
}

pub static JA: Strings = Strings {
    thousands_separator: ',',

    menu_file: "ファイル",
    menu_edit: "編集",
    menu_view: "表示",
    menu_tools: "ツール",
    menu_help: "ヘルプ",

    new_project: "新規プロジェクト…",
    open: "プロジェクトを開く…",
    recent: "最近使ったプロジェクト",
    recent_empty: "（なし）",
    import: "点群を取り込む…",
    export: "書き出し…",
    quit: "終了",
    undo: "元に戻す",
    redo: "やり直す",
    clear_selection: "選択を解除",
    exclude_selection: "選択した点を移動",
    new_folder: "新しいフォルダー",
    fit_view: "全体表示",
    view_top: "上面",
    view_front: "正面",
    view_side: "側面",
    view_iso: "等角",
    ortho: "平行投影",
    color_by: "色",
    color_rgb: "RGB",
    color_height: "高さ",
    color_scan: "スキャンごと",
    edl: "陰影強調（EDL）",
    save: "保存",
    revisions: "リビジョン一覧…",
    discard: "未保存の変更を破棄",
    cleanup: "履歴のクリーンアップ…",
    export_format: "ファイル形式",
    export_message: "現在の状態（未保存の変更を含む）の全スキャンから、表示中のレイヤーの点を、位置合わせを反映して書き出します。",
    export_las_to_e57_notice: "LAS/LAZ由来の分類・リターン情報・GPS時刻・Extra BytesなどはE57には書き出せません。これらの属性を保持するにはLAS/LAZを選んでください。",
    filter_memory: "処理メモリの予算",
    filter_memory_hint: "予算に合わせて並列処理を調整します。点群の表示などには別にメモリを使用します。",
    export_e57_message: "1つのE57ファイルにスキャンと対応画像を保持します。スキャンと画像の姿勢には、フォルダーとスキャン自身の変換を反映します。",
    export_las_message: "LAS/LAZは変換を適用した共通座標で書き出します。E57由来の強度と色は16ビットに換算します。LAS標準属性の有無が違う場合、欠けているGPS時刻・近赤外値は0、RGBは表示色で補います。",
    export_preserve_attributes: "すべての属性を保持する",
    export_omit_incompatible: "不一致の属性・付加情報を除いてまとめる",
    export_conflicts: "以下の項目が一致しません。除外してまとめるか、スキャンごとの書き出しを選んでください。元のプロジェクトは変更されません。",
    export_omit_extra: "除外: Extra Bytes（拡張属性）全体とその定義",
    export_omit_gps: "除外: GPS時刻（方式が不一致）。出力形式で必須の場合は0に置換します。",
    export_omit_crs: "除外: 座標系情報。座標は自動変換しません。スキャンを共通座標に合わせてから書き出してください。",
    export_no_conflicts: "不一致はありません。属性を保持して書き出せます。",
    export_merged: "1つのファイルにまとめる",
    export_per_scan: "スキャンごとに別のファイル（フォルダーを選択）",
    export_button: "書き出す",
    subsample: "ボクセル間引き…",
    remove_noise: "ノイズ除去…",
    remove_outliers: "統計的外れ値除去（SOR）…",
    outliers_message: "各点から近い順に指定数の点までの平均距離を求め、スキャン全体の平均より標準偏差の指定倍以上離れている点を移動先のレイヤーへ移します。探索範囲の中に指定数の点がない点も移します。全体の統計を求めてから判定するため、点群を2回読みます。",
    outlier_neighbours: "近傍点数",
    outlier_deviations: "標準偏差の倍数",
    outlier_reach: "探索範囲（m）",
    subsample_message: "ボクセルごとに、中心に最も近い元の点を1つ残します。点の位置・色は変えません。間引いた点は移動先のレイヤーへ移り、「点群」へ戻せば元に戻ります。",
    noise_message: "指定した半径の中にある他の点が指定数より少ない孤立点を、移動先のレイヤーへ移します。移した点は「点群」へ戻せます。",
    voxel_size: "ボクセルの大きさ（m）",
    search_radius: "探索半径（m）",
    min_neighbours: "最小近傍点数",
    filter_targets: |scans| {
        format!("表示中の {scans} スキャンの、表示中のレイヤーの点に、スキャンごとに適用します。")
    },
    run: "実行",
    points_moved: |points, layer| format!("{points} 点を「{layer}」へ移動しました。"),
    status_no_change: "移動する点はありませんでした。",
    shortcuts: "キーボードショートカット",
    about: "Geemil Workbenchについて",

    navigate: "カメラ操作",
    tool_rect: "矩形選択",
    tool_polygon: "多角形選択",
    tool_measure: "距離計測",
    tool_align: "位置合わせ",
    tool_box: "3Dボックス選択",
    tool_transform: "移動・回転",
    hint_transform: "ツリーで選んだスキャンかフォルダーを、矢印のドラッグで平行移動、輪のドラッグで回転（箱の中心まわり）　離すと確定（Ctrl+Zで元に戻す）　Esc: ドラッグを取り消す　左ドラッグ（他の場所）: 回転",
    hint_box: "箱の矢印: 移動　輪: 回転　サイズ変更: 面の取っ手をドラッグ　Esc: 操作を取り消す　左ドラッグ（他の場所）: カメラ回転",
    box_title: "3Dボックス選択",
    box_hint: "箱の矢印をドラッグして移動、輪で回転、面の四角い取っ手でサイズ変更。Escで操作を取り消せます。",
    box_to_view: "視点の中心へ",
    box_to_view_hint: "箱の中心を回転中心（画面の中央）に移します。ダブルクリックで回転中心を決めてから使います。",
    box_reset: "表示に合わせて置き直す",
    box_highlight: "箱の中をハイライトする",
    box_exclude_outside: "箱の外を移動（切り出し）",
    box_exclude_inside: "箱の中を移動",
    hint_align: "動かすスキャンかフォルダーをツリーで選び、右のパネルで対応点かICPで合わせる　左ドラッグ: 回転　クリック: 対応点　Backspace: 最後の対応点を取り消す　Esc: 対応点を消去",
    align_title: "位置合わせ",
    align_choose_item: "動かすスキャンかフォルダーをプロジェクトツリーで選択してください。表示中の他のスキャンが基準になります。",
    align_moving: "動かす側",
    align_reference: "基準",
    align_reference_scans: |scans| format!("表示中の他のスキャン {scans} 件"),
    align_tint: "動かす側と基準を色分けする",
    align_show: "表示",
    align_show_both: "両方",
    align_pairs: "1. 対応点で大まかに合わせる",
    align_pairs_hint: "動かす側と基準で同じ場所をクリックします（順番は自由）。3組以上で合わせられます。",
    align_residual: "残差",
    remove: "削除",
    align_fit_pairs: "対応点で合わせる",
    align_need_three: "動かす側と基準の両方がそろった対応点が3組以上必要です。",
    align_clear_pairs: "すべて消去",
    align_icp: "2. ICPで微調整",
    align_icp_hint: "重なる部分の形状で合わせます。最大対応距離は残っているずれより大きくします。",
    align_icp_distance: "最大対応距離（m）",
    align_icp_samples: "サンプル点数",
    align_run_icp: "ICPで微調整",
    align_pairs_result: |rms| format!("対応点の残差（RMS）: {rms:.3} m"),
    align_icp_result: |rms, overlap, iterations| {
        format!("ICP: RMS {rms:.4} m・重なり {overlap:.0}%・反復 {iterations} 回")
    },
    align_low_overlap: "重なりが少ないため、結果が不確かです。",
    align_discard: "破棄",
    hint_navigate: "左ドラッグ: 回転　右ドラッグ: 平行移動　ホイール: 拡大縮小　ダブルクリック: 回転中心",
    hint_rect: "ドラッグで矩形を選択　Delete: 選択した点を移動　Esc: 選択を解除",
    hint_polygon: "クリックで頂点を追加　ダブルクリック／Enter: 閉じる　Delete: 選択した点を移動　Esc: 選択を解除",
    hint_measure: "2点をクリックして距離を計測　Esc: 計測をやり直す",
    exclude_inside: "範囲内",
    exclude_outside: "範囲外（切り出し）",
    depth: "奥行きを制限",
    preview_count: |points| format!("移動予定 {points} 点（表示中の点）"),
    preview_searching: "手前の点を確認中",
    measure_result: |d, h, v| format!("距離 {d:.3} m　水平 {h:.3} m　高低差 {v:+.3} m"),

    point_budget: "描画点数",
    point_size: "点サイズ",
    edl_strength: "強度",

    no_project_hint: "プロジェクトを作成するか開いてください。",
    scan_points: |points| format!("{points} 点"),
    folder_summary: |scans, points| format!("{scans} スキャン・{points} 点"),
    rename: "名前を変更",
    move_to: "移動先",
    top_level: "（最上位）",
    ungroup: "フォルダーを解除",
    remove_scans: "プロジェクトから外す",
    show: "表示する",
    hide: "非表示にする",
    show_only: "これだけ表示",
    show_all: "すべて表示",
    drop_to_top: "ここにドロップすると最上位へ移動",
    default_folder_name: "新しいフォルダー",
    properties: "プロパティ",
    tree_selected: |n| format!("選択: {n} 項目"),
    nothing_selected: "ツリーでスキャンかフォルダーを1つ選択してください。",
    prop_name: "名前",
    prop_points: "点数",
    prop_valid: "有効点",
    prop_chunks: "チャンク",
    prop_source: "元ファイル",
    prop_scans: "スキャン数",
    images: "対応画像",
    unnamed_image: "画像",
    omitted_attributes: "取り込み時に省略した属性",
    transform: "位置合わせ（追加変換）",
    transform_folder_hint: "フォルダー内のすべてのスキャンと画像に適用します。",
    translation: "平行移動（m）",
    rotation: "回転（度）",
    apply: "適用",
    reset: "リセット",
    transform_previewing: "未適用の変更を表示しています。「適用」で確定します。",
    revert: "入力を戻す",
    subsample_merged: "表示中のスキャンをまとめて間引く",
    subsample_merged_hint: "共通座標の1つのグリッドで間引き、スキャンの重なる部分でも1ボクセルに1点だけ残します。位置合わせの後、書き出す前に使います。",

    layers: "レイヤー",
    layers_hint: "すべての点はいずれか1つのレイヤーに属します。表示中のレイヤーの点が、表示・選択・処理・書き出しの対象です。",
    default_layer: "点群",
    layer_deleted: "削除",
    layer_noise: "ノイズ",
    layer_subsampled: "間引き",
    new_layer: "新しいレイヤー",
    new_layer_title: "新しいレイヤー",
    new_layer_suffix: |name| format!("{name}（新規）"),
    destination: "移動先",
    move_all_points: "すべての点を移動",
    delete_layer: "レイヤーを削除（点は「点群」へ戻す）",

    revisions_title: "リビジョン",
    revisions_lead: "開くリビジョンや未保存の変更を選んでください。ダブルクリックでも開けます。",
    unsaved_state: "未保存の変更",
    open_revision: "このリビジョンを開く",
    delete_revision: "削除",
    shown_revision: "表示中",
    close: "閉じる",
    default_revision_name: |n| format!("リビジョン {n}"),
    revision_created: "プロジェクト作成",
    operation_kind: |kind| match kind {
        "import" => "取り込み",
        "selection" => "範囲選択の移動",
        "layer_visibility" => "レイヤーの表示",
        "create_layer" | "rename_layer" | "move_layer" | "delete_layer" => "レイヤー編集",
        "transform" => "位置合わせ",
        "subsample" => "間引き",
        "noise_filter" => "ノイズ除去",
        "outlier_filter" => "外れ値除去",
        "box" => "ボックス切り出し",
        "create_group" | "rename_group" | "move" | "ungroup" => "ツリー編集",
        "remove_scans" => "スキャンを外す",
        _ => "その他",
    },
    save_title: "リビジョンを保存",
    name: "名前",
    save_button: "保存",
    cancel: "キャンセル",
    discard_title: "未保存の変更があります",
    discard_message: "未保存の変更を破棄します。元に戻すことはできません。",
    discard_and_continue: "破棄して続ける",
    cleanup_title: "履歴のクリーンアップ",
    cleanup_message: "保存済みのリビジョンと現在の作業状態のどれからも使われていないスキャン・点のレイヤー情報・一時ファイルを削除し、ディスク容量を空けます。不要なリビジョンは先にリビジョン一覧で削除してください。元に戻す・やり直すの履歴は消去されます。",
    cleanup_run: "クリーンアップを実行",
    cleanup_done: |report, size| {
        format!(
            "クリーンアップしました: スキャン {} 件・レイヤー情報 {} 件・{} ファイル（{size}）を削除",
            report.scans, report.labels, report.files
        )
    },

    new_project_title: "新規プロジェクト",
    project_name: "プロジェクト名",
    location: "保存先",
    browse: "参照…",
    create: "作成",
    folder_will_be: |path| format!("作成するフォルダー: {path}"),
    import_after: |files| format!("作成後に {files} 件のファイルを取り込みます。"),
    welcome_lead: "大規模点群の後処理ワークベンチ",
    recent_projects: "最近使ったプロジェクト",
    drop_hint: "点群ファイル（E57・LAS・LAZ）をウィンドウにドロップしても始められます。",
    dropped_without_project: "取り込み先のプロジェクトを作成してください。",
    empty_project: "プロジェクトにはまだ点群がありません。E57・LAS・LAZ を取り込むか、ウィンドウにドロップしてください。",

    status_start: "プロジェクトを作成するか、既存のプロジェクトを開いてください。",
    status_opened: "プロジェクトを開きました。",
    status_working: "処理中…",
    status_done: "完了しました。",
    status_failed: "処理を終了しました。",
    status_cancelled: "キャンセルしました。完了済みの処理は残っています。",
    status_saved: |name| format!("リビジョン「{name}」を保存しました。"),
    status_unsaved: "未保存の変更あり",
    status_revision: |name| format!("リビジョン: {name}"),
    view_stats: |points, ms| format!("表示 {points}　更新 {ms:.1} ms"),
    dismiss: "閉じる",
    details: "詳細",
    job_panicked: "処理スレッドで予期しないエラーが発生しました。直前の状態は保持されています。",
    unexpected_error: "処理中にエラーが発生しました。",
    shortcut_rows: &[
        ("Ctrl+N", "新規プロジェクト"),
        ("Ctrl+O", "プロジェクトを開く"),
        ("Ctrl+I", "点群を取り込む"),
        ("Ctrl+S", "保存"),
        ("Ctrl+Z", "元に戻す"),
        ("Ctrl+Y / Ctrl+Shift+Z", "やり直す"),
        ("V", "カメラ操作"),
        ("R", "矩形選択"),
        ("P", "多角形選択"),
        ("M", "距離計測"),
        ("A", "位置合わせ"),
        ("B", "3Dボックス選択（ハイライト・切り出し）"),
        ("T", "移動・回転（矢印で平行移動、輪で回転）"),
        ("Delete", "選択した点を移動"),
        ("Esc", "選択・計測・対応点を解除"),
        ("Enter", "多角形を閉じる"),
        ("Backspace", "最後の対応点を取り消す（位置合わせ）"),
        ("F", "全体表示"),
        ("7 / 1 / 3 / 5", "上面・正面・側面・等角"),
        ("E", "陰影強調（EDL）の切り替え"),
        ("O", "平行投影と透視投影の切り替え"),
        ("左ドラッグ", "回転（カメラ操作・距離計測・位置合わせ）"),
        ("右ドラッグ", "平行移動"),
        ("ホイール", "拡大縮小"),
        ("ダブルクリック", "クリックした点を回転中心にする"),
    ],
    about_text: "Geemil Workbench — 大規模点群をout-of-coreで扱うOSSの後処理ツール。\nライセンス: Apache-2.0　アイコン: Phosphor Icons（MIT）",

    stage: |stage| match stage {
        Stage::ReadingE57 => "E57を読み込み中",
        Stage::ReadingLas => "LAS/LAZを読み込み中",
        Stage::Images => "画像を取り込み中",
        Stage::Partitioning => "空間分割",
        Stage::Indexing => "インデックス構築",
        Stage::BuildingViewTree => "表示用の8分木を構築中",
        Stage::SelectionNearestDepth => "選択範囲の手前の点を検索中",
        Stage::SelectionMove => "選択範囲の点を移動中",
        Stage::MovingLayer => "レイヤーの点を移動中",
        Stage::Subsampling => "間引き中",
        Stage::NoiseFilter => "ノイズを判定中",
        Stage::BoxCrop => "ボックスで判定中",
        Stage::OutlierStatistics => "外れ値: 統計を計算中",
        Stage::OutlierFilter => "外れ値: 判定中",
        Stage::WritingE57 => "E57を書き出し中",
        Stage::WritingLas => "LAS/LAZを書き出し中",
        Stage::IcpSampling => "ICP: 点を抽出中",
        Stage::IcpIterations => "ICP: 反復計算中",
        Stage::ViewPoints => "表示点を読み込み中",
    },
    core_error: |error| {
        match error {
        CoreError::Cancelled => "キャンセルしました。".into(),
        CoreError::ProjectLocked(p) => format!("このプロジェクトは別のアプリで開かれています。そちらを閉じてから開き直してください。\n{}", p.display()),
        CoreError::LasMetadataMismatch => "LASの属性定義・付加情報・GPS時刻の方式が異なります。書き出し画面で不一致の属性を除くか、スキャンごとの書き出しを選んでください。".into(),
        CoreError::UnsupportedLasWaveform => "波形パケットを含むLAS/LAZの取り込みにはまだ対応していません。".into(),
        CoreError::FilterMemoryBudgetTooSmall => "処理メモリの予算が不足しています。フィルター画面で予算を増やしてから実行してください。".into(),
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
            "座標系の異なるスキャンを一つのファイルへ書き出すことはできません。".into()
        }
        CoreError::NoOverlap => {
            "基準のスキャンと重なる点が見つかりません。対応点で大まかに合わせるか、最大対応距離を大きくしてください。".into()
        }
        CoreError::AlignmentUndetermined => {
            "重なる部分の形状（平面だけなど）から位置が決まりません。対応点で合わせてください。".into()
        }
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

    #[test]
    fn byte_sizes_use_binary_units() {
        assert_eq!(JA.bytes(512), "512 B");
        assert_eq!(JA.bytes(1536), "1.5 KiB");
        assert_eq!(JA.bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
