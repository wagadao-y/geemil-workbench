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
    pub export_e57: &'static str,
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
    pub save: &'static str,
    pub save_as: &'static str,
    pub revisions: &'static str,
    pub discard: &'static str,
    pub cleanup: &'static str,
    pub compress_storage: &'static str,
    pub export_las: &'static str,
    pub export_las_message: &'static str,
    pub export_merged: &'static str,
    pub export_per_scan: &'static str,
    pub export_compress: &'static str,
    pub export_button: &'static str,
    pub subsample: &'static str,
    pub remove_noise: &'static str,
    pub subsample_message: &'static str,
    pub noise_message: &'static str,
    pub voxel_size: &'static str,
    pub search_radius: &'static str,
    pub min_neighbours: &'static str,
    pub filter_targets: fn(scans: usize) -> String,
    pub run: &'static str,
    pub layer_added: fn(label: &str) -> String,
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
    pub hint_box: &'static str,
    pub box_title: &'static str,
    pub box_hint: &'static str,
    pub box_center: &'static str,
    pub box_size: &'static str,
    pub box_yaw: &'static str,
    pub box_to_view: &'static str,
    pub box_to_view_hint: &'static str,
    pub box_reset: &'static str,
    pub box_thickness: &'static str,
    pub box_clip: &'static str,
    pub box_exclude_outside: &'static str,
    pub box_exclude_inside: &'static str,
    pub box_inside_layer: fn(points: &str) -> String,
    pub box_outside_layer: fn(points: &str) -> String,
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
    pub show_all: &'static str,
    pub drop_to_top: &'static str,
    pub default_folder_name: &'static str,
    pub properties: &'static str,
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
    pub layers: &'static str,
    pub no_layers: &'static str,
    pub exclusion_layer: fn(points: &str) -> String,
    pub subsample_layer: fn(size: &str, points: &str) -> String,
    pub noise_layer: fn(radius: &str, neighbours: u32, points: &str) -> String,

    // Revisions.
    pub revisions_title: &'static str,
    pub revisions_lead: &'static str,
    pub unsaved_state: &'static str,
    pub continue_unsaved: &'static str,
    pub open_revision: &'static str,
    pub delete_revision: &'static str,
    pub shown_revision: &'static str,
    pub close: &'static str,
    pub default_revision_name: fn(n: usize) -> String,
    pub revision_created: &'static str,
    pub revision_import: fn(file: &str) -> String,
    pub revision_exclude: fn(points: &str) -> String,
    pub revision_crop: fn(points: &str) -> String,
    pub revision_layer: fn(layer: &str, enabled: bool) -> String,
    pub revision_transform: fn(name: &str) -> String,
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
    export_e57: "E57に書き出す…",
    quit: "終了",
    undo: "元に戻す",
    redo: "やり直す",
    clear_selection: "選択を解除",
    exclude_selection: "除外を実行",
    new_folder: "新しいフォルダー",
    fit_view: "全体表示",
    view_top: "上面",
    view_front: "正面",
    view_side: "側面",
    view_iso: "等角",
    ortho: "平行投影",
    edl: "陰影強調（EDL）",
    save: "保存（新しいリビジョン）",
    save_as: "名前を付けて保存…",
    revisions: "リビジョン一覧…",
    discard: "未保存の変更を破棄",
    cleanup: "履歴のクリーンアップ…",
    compress_storage: "旧形式のデータを圧縮",
    export_las: "LAS/LAZに書き出す…",
    export_las_message: "現在の状態（未保存の変更を含む）の全スキャンから除外点を取り除き、位置合わせを適用した座標で書き出します。強度と色は元の値を16ビットに換算します。座標系（WKT）があるときはLAS 1.4、ないときはLAS 1.2になります。",
    export_merged: "1つのファイルにまとめる",
    export_per_scan: "スキャンごとに別のファイル（フォルダーを選択）",
    export_compress: "LAZで圧縮する",
    export_button: "書き出す",
    subsample: "ボクセル間引き…",
    remove_noise: "ノイズ除去…",
    subsample_message: "ボクセルごとに、中心に最も近い元の点を1つ残します。点の位置・色は変えません。間引いた点は除外レイヤーになり、後から解除できます。",
    noise_message: "指定した半径の中にある他の点が指定数より少ない孤立点を除外します。除外した点は除外レイヤーになり、後から解除できます。",
    voxel_size: "ボクセルの大きさ（m）",
    search_radius: "探索半径（m）",
    min_neighbours: "最小近傍点数",
    filter_targets: |scans| format!("表示中の {scans} スキャンに、スキャンごとに適用します。"),
    run: "実行",
    layer_added: |label| format!("除外レイヤーを追加しました: {label}"),
    status_no_change: "除外する点はありませんでした。",
    shortcuts: "キーボードショートカット",
    about: "Geemil Workbenchについて",

    navigate: "カメラ操作",
    tool_rect: "矩形選択",
    tool_polygon: "多角形選択",
    tool_measure: "距離計測",
    tool_align: "位置合わせ",
    tool_box: "3Dボックス",
    hint_box: "右のパネルで箱の位置・大きさ・向きを決め、表示を箱の中に限定（断面）したり、箱の外か中を除外する　左ドラッグ: 回転",
    box_title: "3Dボックス",
    box_hint: "座標はプロジェクトの共通座標です。値はドラッグでも変えられます。",
    box_center: "中心（m）",
    box_size: "大きさ（m）",
    box_yaw: "Z軸回転（度）",
    box_to_view: "視点の中心へ",
    box_to_view_hint: "箱の中心を回転中心（画面の中央）に移します。ダブルクリックで回転中心を決めてから使います。",
    box_reset: "表示に合わせて置き直す",
    box_thickness: "高さ（Z）",
    box_clip: "表示を箱の中に限定する（断面表示）",
    box_exclude_outside: "箱の外を除外（切り出し）",
    box_exclude_inside: "箱の中を除外",
    box_inside_layer: |points| format!("ボックス内を除外（{points} 点）"),
    box_outside_layer: |points| format!("ボックス外を除外（{points} 点）"),
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
    hint_rect: "ドラッグで矩形を選択　Delete: 除外を実行　Esc: 選択を解除",
    hint_polygon: "クリックで頂点を追加　ダブルクリック／Enter: 閉じる　Delete: 除外を実行　Esc: 選択を解除",
    hint_measure: "2点をクリックして距離を計測　Esc: 計測をやり直す",
    exclude_inside: "範囲内を除外",
    exclude_outside: "範囲外を除外",
    depth: "奥行きを制限",
    preview_count: |points| format!("除外予定 {points} 点（表示中の点）"),
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
    show_only: "これだけ表示",
    show_all: "すべて表示",
    drop_to_top: "ここにドロップすると最上位へ移動",
    default_folder_name: "新しいフォルダー",
    properties: "プロパティ",
    nothing_selected: "ツリーでスキャンかフォルダーを選択してください。",
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
    layers: "除外レイヤー",
    no_layers: "除外レイヤーはありません。",
    exclusion_layer: |points| format!("除外（{points} 点）"),
    subsample_layer: |size, points| format!("間引き {size} m（{points} 点）"),
    noise_layer: |radius, neighbours, points| {
        format!("ノイズ除去 {radius} m・{neighbours} 点未満（{points} 点）")
    },

    revisions_title: "リビジョン",
    revisions_lead: "保存したリビジョンの一覧です。開くリビジョンを選んでください。",
    unsaved_state: "未保存の変更",
    continue_unsaved: "未保存の作業を続ける",
    open_revision: "このリビジョンを開く",
    delete_revision: "削除",
    shown_revision: "表示中",
    close: "閉じる",
    default_revision_name: |n| format!("リビジョン {n}"),
    revision_created: "プロジェクト作成",
    revision_import: |file| format!("取り込み: {file}"),
    revision_exclude: |points| format!("除外: {points} 点"),
    revision_crop: |points| format!("範囲外を除外: {points} 点"),
    revision_layer: |layer, enabled| {
        if enabled {
            format!("レイヤーを有効化: {layer}")
        } else {
            format!("レイヤーを無効化: {layer}")
        }
    },
    revision_transform: |name| format!("位置合わせ: {name}"),
    operation_kind: |kind| match kind {
        "import" => "取り込み",
        "selection" => "除外",
        "layer" => "レイヤー切り替え",
        "transform" => "位置合わせ",
        "subsample" => "間引き",
        "noise_filter" => "ノイズ除去",
        "box" => "ボックス切り出し",
        "create_group" | "rename_group" | "move" | "ungroup" => "ツリー編集",
        "remove_scans" => "スキャンを外す",
        _ => "その他",
    },
    save_title: "名前を付けて保存",
    name: "名前",
    save_button: "保存",
    cancel: "キャンセル",
    discard_title: "未保存の変更があります",
    discard_message: "未保存の変更を破棄します。元に戻すことはできません。",
    discard_and_continue: "破棄して続ける",
    cleanup_title: "履歴のクリーンアップ",
    cleanup_message: "保存済みのリビジョンと現在の作業状態のどれからも使われていないスキャン・除外レイヤー・一時ファイルを削除し、ディスク容量を空けます。不要なリビジョンは先にリビジョン一覧で削除してください。元に戻す・やり直すの履歴は消去されます。",
    cleanup_run: "クリーンアップを実行",
    cleanup_done: |report, size| {
        format!(
            "クリーンアップしました: スキャン {} 件・除外レイヤー {} 件・{} ファイル（{size}）を削除",
            report.scans, report.layers, report.files
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
        ("Ctrl+S", "保存（新しいリビジョン）"),
        ("Ctrl+Shift+S", "名前を付けて保存"),
        ("Ctrl+Z", "元に戻す"),
        ("Ctrl+Y / Ctrl+Shift+Z", "やり直す"),
        ("V", "カメラ操作"),
        ("R", "矩形選択"),
        ("P", "多角形選択"),
        ("M", "距離計測"),
        ("A", "位置合わせ"),
        ("B", "3Dボックス（断面表示・切り出し）"),
        ("Delete", "除外を実行"),
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
        Stage::BuildingParentLod => "LODを構築中",
        Stage::CompressingPoints => "点データを圧縮中",
        Stage::CompressingLod => "LODを圧縮中",
        Stage::VerifyingCompressedPoints => "圧縮データを検証中",
        Stage::SelectionNearestDepth => "選択範囲の手前の点を検索中",
        Stage::SelectionExclusionMask => "除外範囲を作成中",
        Stage::Subsampling => "間引き中",
        Stage::NoiseFilter => "ノイズを判定中",
        Stage::BoxCrop => "ボックスで判定中",
        Stage::WritingE57 => "E57を書き出し中",
        Stage::WritingLas => "LAS/LAZを書き出し中",
        Stage::IcpSampling => "ICP: 点を抽出中",
        Stage::IcpIterations => "ICP: 反復計算中",
        Stage::ViewLod => "表示LODを選択中",
        Stage::ViewPoints => "表示点を読み込み中",
    },
    core_error: |error| {
        match error {
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
