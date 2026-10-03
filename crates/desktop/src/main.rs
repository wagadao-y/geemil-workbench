mod app;
mod i18n;
mod render;

fn main() -> eframe::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let project = args
        .first()
        .filter(|a| !a.to_string_lossy().starts_with("--"))
        .map(std::path::PathBuf::from);
    let smoke = app::SmokeOptions {
        screenshot: args
            .iter()
            .position(|a| a == "--smoke-test")
            .and_then(|i| args.get(i + 1))
            .map(std::path::PathBuf::from),
        orbit: args.iter().any(|a| a == "--smoke-orbit"),
        colors: args.iter().any(|a| a == "--smoke-colors"),
        script: args
            .iter()
            .position(|a| a == "--smoke-script")
            .and_then(|i| args.get(i + 1))
            .map(|s| s.to_string_lossy().split(',').map(str::to_owned).collect())
            .unwrap_or_default(),
        dialog: args
            .iter()
            .position(|a| a == "--smoke-dialog")
            .and_then(|i| args.get(i + 1))
            .map(|d| d.to_string_lossy().into_owned()),
        select: args
            .iter()
            .position(|a| a == "--smoke-select")
            .and_then(|i| args.get(i + 1))
            .and_then(|mode| match mode.to_str() {
                Some("inside") => Some((geemil_core::SelectionMode::ExcludeInside, true)),
                Some("inside-all") => Some((geemil_core::SelectionMode::ExcludeInside, false)),
                Some("outside") => Some((geemil_core::SelectionMode::ExcludeOutside, false)),
                _ => None,
            }),
    };
    // Smoke tests keep a fixed window and leave saved window state alone.
    let interactive = smoke.screenshot.is_none() && !smoke.colors;
    let mut gpu = eframe::egui_wgpu::WgpuConfiguration::default();
    // Use the Windows graphics API without probing Vulkan drivers on startup.
    // WGPU_BACKEND remains available for diagnosing a different backend.
    if cfg!(target_os = "windows")
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut gpu.wgpu_setup
    {
        setup.instance_descriptor.backends = wgpu::Backends::DX12.with_env();
    }
    eframe::run_native(
        "Geemil Workbench",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            wgpu_options: gpu,
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1280., 800.])
                .with_min_inner_size([800., 500.]),
            persist_window: interactive,
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(app::Workbench::new(cc, project, smoke)))),
    )
}
