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
    };
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
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(app::Workbench::new(cc, project, smoke)))),
    )
}
