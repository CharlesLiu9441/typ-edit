mod app;
mod branding;
mod cache;
mod commands;
mod export;
mod format;
mod i18n;
mod keybindings;
mod media;
mod paste_image;
mod platform;
mod settings;
mod shell;
mod theme;
pub mod ui_fonts;
mod workspace;

use std::path::PathBuf;

/// MSAA sample count of the main render pass.
///
/// Shared with the `typst-gpu-canvas` config: wgpu requires a pipeline's
/// sample count to match the render pass exactly.
pub(crate) const MSAA_SAMPLES: u32 = 4;

/// Launch the Typst editor.
///
/// Reads an optional startup path (a workspace folder or a `.typ` file) from
/// the first command-line argument and runs the native eframe application.
pub fn run() -> eframe::Result {
    env_logger::init();

    // Optional startup path: a workspace folder or a `.typ` file.
    let open: Option<PathBuf> = std::env::args_os().nth(1).map(PathBuf::from);

    // The app draws its own title strip (menus + window buttons), so the OS
    // decorations are disabled; edge resizing is re-implemented in `App::ui`.
    let viewport = egui::ViewportBuilder::default()
        .with_decorations(false)
        .with_icon(branding::window_icon())
        .with_inner_size([1280.0, 800.0])
        // The three-card workspace and the 620x440 settings overlay need
        // room; below this the chrome starts to collide rather than reflow.
        .with_min_inner_size([900.0, 600.0]);
    #[cfg(target_os = "macos")]
    let viewport = viewport.with_transparent(true);

    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        multisampling: MSAA_SAMPLES as u16,
        viewport,
        stencil_buffer: 8,
        ..Default::default()
    };

    eframe::run_native(
        "Typst Editor",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::TypEditApp::new(cc, open)))),
    )
}
