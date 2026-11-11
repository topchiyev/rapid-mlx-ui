mod backend;
mod demo;
mod frontend;
mod mcp_bridge;
mod persistence;
mod turn;
mod view;

fn main() -> eframe::Result {
    let demo = std::env::args().any(|arg| arg == "--demo")
        || std::env::var("RAPID_MLX_UI_DEMO").as_deref() == Ok("1");
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Rapid MLX UI")
            .with_inner_size([1320.0, 860.0])
            .with_min_inner_size([960.0, 640.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    let (tx, rx, thread) = if demo {
        let (tx, rx) = demo::spawn();
        (tx, rx, None)
    } else {
        let (tx, rx, thread) = backend::Backend::start().into_parts();
        (tx, rx, Some(thread))
    };
    let shutdown = tx.clone();
    let result = eframe::run_native(
        "Rapid MLX UI",
        options,
        Box::new(move |cc| Ok(Box::new(frontend::Frontend::new(cc, tx, rx, demo)))),
    );
    if let Some(thread) = thread {
        backend::Backend::finish(thread, shutdown);
    }
    result
}
