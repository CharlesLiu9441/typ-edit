use super::files::classify_startup_path;
use super::frame_tree::FrameTreeState;
use super::outline::OutlineState;
use super::recovery::Recovery;
use super::search::{LeftView, SearchState};
use super::spell::SpellState;
use super::vector_preview::{GpuCanvasHandle, VectorPreview};
use super::{
    DocumentController, EditorTab, ExportState, GalleryState, HoverState, NewProjectDialog,
    PreviewController, SessionState, StartupTarget, TreeDialog, TypEditApp, UiState,
};
use crate::keybindings::KeyBindings;
use crate::settings::{Settings, SettingsLoad, SettingsNotice};
use crate::shell::layout::ColumnLayout;
use crate::shell::tree::ProjectTree;
use crate::theme;
use crossbeam_channel::bounded;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use typ_edit_engine::Engine;
use typ_edit_engine::fonts::Fonts;
#[cfg(feature = "gpu-canvas")]
use typ_edit_i18n::t_static;
use typ_edit_i18n::{t, t_with};
use typ_edit_preview::PreviewState;

/// The first-frame toast for a settings load notice, or `None` on a clean load.
fn settings_notice_toast(notice: Option<&SettingsNotice>) -> Option<String> {
    match notice? {
        SettingsNotice::Corrupt { quarantined, .. } => Some(match quarantined {
            Some(path) => t_with(
                "toast.settings_corrupt_quarantined",
                &[("file", &path.display().to_string())],
            ),
            None => t("toast.settings_corrupt").into_owned(),
        }),
        SettingsNotice::FutureVersion { .. } => {
            Some(t("toast.settings_future_version").into_owned())
        }
        SettingsNotice::Unreadable { .. } => Some(t("toast.settings_unreadable").into_owned()),
    }
}

/// Wraps an egui context into the engine's UI wake handle.
pub(super) fn egui_waker(ctx: egui::Context) -> typ_edit_engine::Waker {
    Arc::new(move || ctx.request_repaint())
}

impl TypEditApp {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        let SettingsLoad { settings, notice } = Settings::load();
        let settings_toast = settings_notice_toast(notice.as_ref());
        match &notice {
            Some(SettingsNotice::Corrupt { quarantined, error }) => match quarantined {
                Some(path) => {
                    log::warn!(
                        "settings file corrupt ({error}); moved to {}",
                        path.display()
                    );
                }
                None => log::warn!("settings file corrupt ({error}); not quarantined"),
            },
            Some(SettingsNotice::FutureVersion { found }) => {
                log::warn!("settings file schema version {found} is newer; writes disabled");
            }
            Some(SettingsNotice::Unreadable { error }) => {
                log::warn!("settings file unreadable: {error}");
            }
            None => {}
        }
        crate::i18n::apply_language(&settings);
        cc.egui_ctx
            .set_theme(theme::preference(settings.appearance.theme));
        // Patch both stored styles: `set_theme` only switches which one is active.
        cc.egui_ctx.all_styles_mut(|style| {
            theme::install_card_style(&mut style.visuals);
            theme::install_text_styles(style, settings.fonts.ui_font_size);
        });
        // Scan the system fonts on a worker thread: the first frame uses egui's
        // built-ins + the bundled math faces, and `refresh_fonts_if_ready`
        // installs the configured families when the scan lands.
        let fonts = Fonts::start();
        let catalog = fonts.current();
        let (ui_ok, editor_ok) = crate::ui_fonts::install_egui_fonts(
            &cc.egui_ctx,
            &settings.fonts.ui_font,
            &settings.fonts.editor_font,
            &catalog,
        );
        if fonts.is_ready() && (!ui_ok || !editor_ok) {
            log::warn!("font fallback in use (ui_ok={ui_ok}, editor_ok={editor_ok})");
        }
        drop(catalog);

        // egui's cross-pass rect-vs-id heuristic paints spurious red frames whenever a
        // relayout reuses a rect (e.g. collapsing panes). Upstream rated the false positives
        // worse than the signal and disabled it by default in egui 0.36; do the same here.
        // The real duplicate-id check (`warn_on_id_clash`) stays enabled.
        #[cfg(debug_assertions)]
        cc.egui_ctx
            .global_style_mut(|style| style.debug.warn_if_rect_changes_id = false);

        // With the `gpu-canvas` feature the renderer brings its own stencil-clip
        // pipelines and only needs the host pass to carry the
        // `NativeOptions.stencil_buffer` attachment (and the matching
        // `multisampling` count); an adapter that cannot host that pass yields
        // `None`. Without the feature the preview always uses raster.
        #[cfg(feature = "gpu-canvas")]
        let gpu_canvas = cc.wgpu_render_state.as_ref().and_then(|render_state| {
            typst_gpu_canvas::GpuCanvas::new(
                render_state,
                typst_gpu_canvas::GpuCanvasConfig {
                    msaa_samples: crate::MSAA_SAMPLES,
                    target_format: render_state.target_format,
                    loading_label: t_static("startup.loading"),
                    image_strategy: typst_gpu_canvas::ImageStrategy::Auto,
                    image_max_side: settings.preview.gpu_image_max_side.clamp(
                        crate::settings::GPU_IMAGE_MAX_SIDE_MIN,
                        crate::settings::GPU_IMAGE_MAX_SIDE_MAX,
                    ),
                },
            )
        });
        #[cfg(not(feature = "gpu-canvas"))]
        let gpu_canvas: Option<GpuCanvasHandle> = None;

        let mut app = Self::new_with(&cc.egui_ctx, gpu_canvas, fonts, settings, open);
        if let Some(toast) = settings_toast {
            app.toast_warning(toast);
        }
        app
    }

    /// Builds the app from already-resolved host resources: the single home of
    /// field initialization, usable without an `eframe::CreationContext`.
    pub(crate) fn new_with(
        egui_ctx: &egui::Context,
        gpu_canvas: Option<GpuCanvasHandle>,
        fonts: Arc<Fonts>,
        settings: Settings,
        open: Option<PathBuf>,
    ) -> Self {
        let mut settings = settings;
        let recent_before = settings.workspace.recent_projects.len();
        settings
            .workspace
            .recent_projects
            .retain(|path| !crate::workspace::is_transient_root(path));
        let recents_pruned = settings.workspace.recent_projects.len() != recent_before;
        // The tui-based window skeleton relies on egui's multipass discard support:
        // with `max_passes == 1` every taffy layout change would only land one frame
        // late (egui 0.36 defaults to 2; tui.rs documents the mechanism).
        debug_assert!(
            egui_ctx.options(|o| o.max_passes.get()) >= 2,
            "egui_taffy layout requires max_passes >= 2"
        );
        // The whole-interface scale is independent of the UI font size. Doing
        // it here (not only in `new`) keeps the headless harness on the same
        // path; text styles are installed here too for the same reason.
        egui_ctx.set_zoom_factor(crate::ui_fonts::ui_zoom_factor(
            settings.appearance.ui_scale,
        ));
        egui_ctx.all_styles_mut(|style| {
            crate::theme::install_card_style(&mut style.visuals);
            crate::theme::install_text_styles(style, settings.fonts.ui_font_size);
        });
        let engine = Engine::spawn(egui_waker(egui_ctx.clone()), Arc::clone(&fonts));
        // Export channel: bounded(2) — at most one export runs at a time (the
        // `exporting` flag), each sends exactly one report, and the app drains
        // every frame, so the capacity is never reached. The export worker's
        // blocking `send` therefore never waits in practice.
        let (export_tx, export_rx) = bounded(2);
        let bindings = KeyBindings::from_settings(&settings);

        let mut app = Self {
            engine,
            fonts,
            settings: settings.clone(),
            settings_dirty: recents_pruned,
            settings_changed_at: None,
            settings_save_failed: false,
            settings_read_only_warned: false,
            bindings,
            ui_ctx: egui_ctx.clone(),
            format_worker: super::format_worker::FormatWorker::default(),
            pending_save: None,
            external_changes: BTreeMap::new(),
            watch_pending: BTreeSet::new(),
            watch_due: None,
            watcher: super::watcher::ProjectWatcher::new(egui_ctx.clone()),
            session: SessionState {
                tabs: vec![EditorTab::new(0)],
                active_tab: 0,
                close_prompt: None,
                allow_close: false,
                next_tab_id: 1,
            },
            compile: DocumentController::default(),
            ide: super::ide::IdeState::default(),
            spell: SpellState::load(),
            left_view: LeftView::default(),
            search: SearchState::default(),
            outline: OutlineState::default(),
            preview: PreviewController::new(PreviewState::new(), VectorPreview::new(gpu_canvas)),
            frame_tree: FrameTreeState::default(),
            tree: ProjectTree::new(None, None),
            project_lock: None,
            layout: ColumnLayout::new(settings.layout.pane_widths, settings.layout.pane_collapsed),
            hover: HoverState {
                completions: {
                    let mut items = typ_edit_engine::completions::keyword_completions();
                    items.extend(
                        typ_edit_engine::completions::library_completions()
                            .iter()
                            .cloned(),
                    );
                    Arc::new(items)
                },
                world_completions: None,
                hover_cache: None,
                hover_anchor: None,
                hover_suppressed: false,
                hover_popup: None,
            },
            export: ExportState {
                rx: export_rx,
                tx: export_tx,
                png_dialog: false,
                exporting: false,
            },
            new_project: NewProjectDialog::default(),
            gallery: GalleryState::default(),
            ui: UiState {
                toast: None,
                last_title: "Typst Editor".to_owned(),
                error_rows: Vec::new(),
                settings_open: false,
                shortcuts_open: false,
                typst_help_open: false,
                palette_open: false,
                palette_query: String::new(),
                palette_selected: 0,
                palette_focus: false,
                tree_dialog: TreeDialog::None,
                window_size_clamped: false,
                fonts_ready: false,
                landing: false,
                worker_degraded: None,
                icon_small: None,
                icon_large: None,
            },
            recovery: Recovery::default(),
            git: super::git::GitState::new(egui_waker(egui_ctx.clone())),
            git_ui: super::git_ui::GitUiState::default(),
            collab: super::collab::CollabController::default(),
            downloads: super::downloads::DownloadState::default(),
        };
        app.apply_startup(open);
        let has_root = app.tree.root.is_some();
        app.recovery.maybe_prompt_startup_recovery(has_root);
        app
    }

    /// Installs the configured families once the background scan lands, and
    /// keeps the UI repainting while it is still loading.
    pub(super) fn refresh_fonts_if_ready(&mut self, ctx: &egui::Context) {
        if self.ui.fonts_ready {
            return;
        }
        if !self.fonts.is_ready() {
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        self.ui.fonts_ready = true;
        let catalog = self.fonts.current();
        let (ui_ok, editor_ok) = crate::ui_fonts::install_egui_fonts(
            ctx,
            &self.settings.fonts.ui_font,
            &self.settings.fonts.editor_font,
            &catalog,
        );
        self.invalidate_editor_font_layout();
        if !ui_ok || !editor_ok {
            log::warn!("font fallback in use (ui_ok={ui_ok}, editor_ok={editor_ok})");
        }
        ctx.request_repaint();
    }

    /// Drops every open tab's shaped-line cache after a font installation.
    pub(super) fn invalidate_editor_font_layout(&mut self) {
        for tab in &mut self.session.tabs {
            tab.editor.invalidate_font_layout();
        }
    }

    /// Applies the startup target: opens the CLI argument when usable; with no
    /// argument the app starts on the plain empty workspace.
    pub(super) fn apply_startup(&mut self, open: Option<PathBuf>) {
        if let Some(path) = open {
            match classify_startup_path(&path) {
                StartupTarget::Workspace(root) => self.open_root(root),
                StartupTarget::File(file) => {
                    // Set the root first: it does not auto-open, so the file is
                    // compiled exactly once.
                    if let Some(parent) = file.parent()
                        && !self.adopt_root(parent)
                    {
                        return;
                    }
                    self.open_file(file);
                }
                StartupTarget::Ignore => {
                    log::warn!("ignoring CLI argument: {}", path.display());
                }
            }
        }
    }
}
