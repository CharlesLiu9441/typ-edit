//! The `eframe::App` frame loop: worker pump, layout, and pane orchestration.
//!
//! Window chrome lives in [`super::window_chrome`] and the landing page in
//! [`super::landing`]; this module only sequences them.

use super::dialogs::ClosePromptOutcome;
use super::dispatch::CommandState;
use super::files::remove_recent;
use super::frame_tree::FrameTreeOutcome;
use super::gallery::GalleryAction;
use super::image_import::show_drop_hint;
use super::menu::MenuOutcome;
use super::recovery::{RecoveryOutcome, RecoveryRestoreStep};
use super::window_chrome::{TITLE_BAR_H, TitleBarInput};
use super::{ClosePrompt, ExportDialogAction, TypEditApp};
use crate::export::ExportFormat;
use crate::shell::layout::{PANE_TITLES, Pane};
use crate::shell::status::{self, StatusInfo};
use crate::shell::tree::TreeAction;
use eframe::epaint::{CornerRadius, Stroke, StrokeKind};
use std::time::{Duration, Instant};
use typ_edit_editor::EditorOutput;
use typ_edit_i18n::{t, t_with};
use typ_edit_preview::{DebugHighlight, PreviewAction};
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::{
    self, Tui, TuiBuilderLogic,
    taffy::{Dimension, Size},
};

/// The pane results one frame produces, handed to the post-render handlers.
struct PaneOutputs {
    tree: TreeAction,
    editor: EditorOutput,
    editor_shown: bool,
    preview: PreviewAction,
}

/// Idle time after the last settings change before the debounced persist runs.
pub(super) const SETTINGS_PERSIST_DEBOUNCE: Duration = Duration::from_millis(800);

/// Whether the debounced settings persist is due: something is pending and the
/// last change has been idle for at least `debounce`. A pending change with no
/// stamp is due immediately.
pub(super) fn settings_persist_due(
    dirty: bool,
    changed_at: Option<Instant>,
    now: Instant,
    debounce: Duration,
) -> bool {
    dirty && changed_at.is_none_or(|changed| now.saturating_duration_since(changed) >= debounce)
}

/// App-only effects of a settings persist attempt, applied by the caller.
#[derive(Default)]
pub(super) struct SettingsPersistOutcome {
    /// A one-shot failure toast to raise, if this attempt started a failure cycle.
    pub(super) toast: Option<String>,
    /// When set, the caller must `request_repaint_after` so an idle app flushes.
    pub(super) repaint_after: Option<Duration>,
}

impl TypEditApp {
    /// Updates the OS window title from the active tab, sending it only on change.
    pub(super) fn update_title(&mut self, ctx: &egui::Context) {
        let title = match self.session.tabs[self.session.active_tab]
            .editor
            .path
            .as_ref()
            .and_then(|path| path.file_name())
        {
            Some(name) => {
                let name = name.to_string_lossy();
                if self.session.tabs[self.session.active_tab].dirty {
                    format!("{name} ● — Typst Editor")
                } else {
                    format!("{name} — Typst Editor")
                }
            }
            None => "Typst Editor".to_owned(),
        };
        if title != self.ui.last_title {
            self.ui.last_title = title;
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.ui.last_title.clone()));
        }
    }

    /// Stops every engine worker via the single shutdown path used on exit and
    /// by tests. After this, `request_*` are no-ops.
    pub(super) fn shutdown_background(&mut self) {
        self.engine.shutdown();
    }

    /// Records that settings changed and stamps the debounce window. The
    /// frame-end persist writes them once the window elapses, so no render-path
    /// code touches the disk directly.
    pub(super) fn mark_settings_dirty(&mut self) {
        self.settings_dirty = true;
        self.settings_changed_at = Some(Instant::now());
    }

    /// Persists pending settings changes when the debounce has elapsed (or when
    /// `force`d, as on exit), returning the toast and repaint the caller applies.
    pub(super) fn persist_settings(&mut self, force: bool) -> SettingsPersistOutcome {
        if !self.settings_dirty {
            return SettingsPersistOutcome::default();
        }
        if !self.settings.is_writable() {
            // The file came from a newer build. Drop the pending change and warn
            // once instead of pretending the save succeeded and retrying forever.
            self.settings_dirty = false;
            self.settings_changed_at = None;
            let toast = if self.settings_read_only_warned {
                None
            } else {
                self.settings_read_only_warned = true;
                Some(t("toast.settings_read_only").into_owned())
            };
            return SettingsPersistOutcome {
                toast,
                repaint_after: None,
            };
        }
        let now = Instant::now();
        if !force
            && !settings_persist_due(
                self.settings_dirty,
                self.settings_changed_at,
                now,
                SETTINGS_PERSIST_DEBOUNCE,
            )
        {
            let remaining = self.settings_changed_at.map_or(Duration::ZERO, |changed| {
                SETTINGS_PERSIST_DEBOUNCE.saturating_sub(now.saturating_duration_since(changed))
            });
            return SettingsPersistOutcome {
                toast: None,
                repaint_after: Some(remaining),
            };
        }
        match self.settings.save() {
            Ok(()) => {
                self.settings_dirty = false;
                self.settings_changed_at = None;
                self.settings_save_failed = false;
                SettingsPersistOutcome::default()
            }
            Err(err) => {
                log::error!("cannot save settings: {err}");
                // Keep the change pending and retry after another debounce.
                self.settings_changed_at = Some(now);
                let toast = if self.settings_save_failed {
                    None
                } else {
                    self.settings_save_failed = true;
                    Some(t_with(
                        "toast.settings_save_failed",
                        &[("err", &err.to_string())],
                    ))
                };
                SettingsPersistOutcome {
                    toast,
                    repaint_after: Some(SETTINGS_PERSIST_DEBOUNCE),
                }
            }
        }
    }

    /// Paints the transient toast and clears it once it expires.
    pub(super) fn show_toast(&mut self, ui: &mut egui::Ui) {
        let Some(toast) = self.ui.toast.clone() else {
            return;
        };
        if toast.at.elapsed() >= Duration::from_secs(8) {
            self.ui.toast = None;
            return;
        }

        let theme = Theme::from_ui(ui);
        let kind = toast.kind;
        let message = toast.message;
        egui::Area::new(egui::Id::new("app_toast"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-18.0, -44.0))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style())
                    // A feedback container carries the opaque semantic surface
                    // and the semantic outline; never use the translucent tint
                    // here or the content behind the toast shows through.
                    .fill(crate::theme::semantic_surface(ui, kind))
                    .stroke(Stroke::new(
                        theme.strokes.card,
                        crate::theme::semantic(ui, kind),
                    ))
                    .show(ui, |ui| {
                        ui.set_max_width(360.0);
                        // Measure the wrapped message first so the tui root
                        // keeps the popup's shrink-to-text width.
                        let font = egui::TextStyle::Body.resolve(ui.style());
                        let galley = ui.painter().layout(
                            message.clone(),
                            font,
                            egui::Color32::PLACEHOLDER,
                            360.0,
                        );
                        let width = (galley.size().x + 1.0).max(1.0);
                        let base = ui.id().with(("typ_edit_toast", "content"));
                        let mut style = tui::column_gap(0.0);
                        tui::fill_width(&mut style);
                        tui::tui(ui, base)
                            .reserve_width(width)
                            .style(style)
                            .show(|tree| {
                                tree.id(base.with("message")).label(message);
                            });
                    });
            });
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }

    /// Applies the app-only effects of a recovery transition in order: restore
    /// steps (open/adopt a tab and fill it dirty), the deferred main file, the
    /// active-tab switch, then the toasts.
    fn apply_recovery_outcome(&mut self, outcome: RecoveryOutcome) {
        let mut active_restored = None;
        for step in outcome.restore {
            let active = match &step {
                RecoveryRestoreStep::File { active, .. }
                | RecoveryRestoreStep::Untitled { active, .. } => *active,
            };
            let index = match step {
                RecoveryRestoreStep::File { path, content, .. } => {
                    self.open_file(path.clone());
                    match self
                        .session
                        .tabs
                        .iter()
                        .position(|tab| tab.editor.path.as_deref() == Some(path.as_path()))
                    {
                        Some(index) => {
                            self.session.tabs[index].editor.set_text(content);
                            self.session.tabs[index].dirty = true;
                            index
                        }
                        None => {
                            let index = self.recovery.new_recovery_tab(content, &mut self.session);
                            self.on_tab_switched();
                            index
                        }
                    }
                }
                RecoveryRestoreStep::Untitled { content, .. } => {
                    let index = self.recovery.new_recovery_tab(content, &mut self.session);
                    self.on_tab_switched();
                    index
                }
            };
            if active {
                active_restored = Some(index);
            }
        }
        if let Some(path) = outcome.open_main {
            self.open_file(path);
        }
        if let Some(index) = active_restored {
            self.activate_tab(index);
        }
        for toast in outcome.toasts {
            self.raise_toast(toast);
        }
    }
}

impl TypEditApp {
    /// [`eframe::App::logic`] without the `eframe::Frame`, so the headless test
    /// harness can drive it; the trait method delegates here.
    pub(super) fn frame_logic(&mut self, ctx: &egui::Context) {
        self.refresh_fonts_if_ready(ctx);
        self.flush_mirror_writes(false);
        if let Some(delay) = self.collab.mirror_write_repaint(Instant::now()) {
            ctx.request_repaint_after(delay);
        }
        if !self.ui.window_size_clamped {
            // Clamp an oversized window once, then never probe again.
            self.ui.window_size_clamped = true;
            let monitor = ctx.input(|i| i.viewport().monitor_size);
            let size = ctx.input(|i| {
                i.viewport()
                    .inner_rect
                    .map(|rect| rect.size())
                    .unwrap_or_else(|| i.content_rect().size())
            });
            if let Some(monitor) = monitor
                && (size.x > monitor.x || size.y > monitor.y)
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
        }
        self.pump(ctx);
        // Collab: flush last frame's captured edits, send due cursors, then drain.
        self.flush_collab_captures();
        self.collab.tick_cursors();
        let collab = self.collab.pump(ctx);
        self.apply_collab_outcome(collab);
        let cursor_byte = self.compile.cursor_byte;
        let git_enabled = self.settings.git.git_enabled;
        self.git_ui
            .maybe_request_blame(&mut self.git, &self.session, cursor_byte, git_enabled);
        let gallery_toasts = self.gallery.pump(ctx);
        for toast in gallery_toasts {
            self.raise_toast(toast);
        }
        // Autosave snapshot of unsaved tabs; cheap `Instant` checks only.
        let root = self.tree.root.clone();
        let recovery = self.recovery.tick(
            ctx,
            &self.session,
            root.as_deref(),
            self.settings.recovery.crash_recovery_enabled,
        );
        for toast in recovery.toasts {
            self.raise_toast(toast);
        }
        // One disk write per frame at most, after every pane has run.
        let persisted = self.persist_settings(false);
        if let Some(toast) = persisted.toast {
            self.toast_error(toast);
        }
        if let Some(delay) = persisted.repaint_after {
            ctx.request_repaint_after(delay);
        }
    }

    /// [`eframe::App::ui`] without the `eframe::Frame`, so the headless test
    /// harness can drive it; the trait method delegates here.
    pub(super) fn frame_ui(&mut self, ui: &mut egui::Ui) {
        // App-level guarantee: while egui has a scroll action in flight
        // (`Smoothing`/`InTouch`, including zero-delta intermediate frames),
        // keep the frame loop hot. Individual editor/preview widgets also guard
        // their own viewports; this covers any other scroll surface as well.
        if ui.ctx().input(|i| i.is_scrolling()) {
            ui.ctx().request_repaint();
        }
        // Intercept the OS close while unsaved buffers would be lost: cancel it
        // and ask. Once the user confirms, `allow_close` lets the next one through.
        if ui.ctx().input(|i| i.viewport().close_requested())
            && !self.session.allow_close
            && self.session.tabs.iter().any(|tab| tab.dirty)
        {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.session.prompt_close(ClosePrompt::App);
        }

        self.import_dropped_files(ui.ctx());

        self.update_title(ui.ctx());

        for id in self.ui.handle_shortcuts(ui, &self.bindings) {
            self.run_command(id, ui.ctx());
        }

        let board = crate::theme::board(ui);
        let ctx = ui.ctx().clone();

        let title_has_root = self.tree.root.is_some();
        let has_document = self.compile.doc.is_some();
        let exporting = self.export.exporting;
        let mut menu = MenuOutcome::default();

        // The window is one taffy column: a fixed 28pt title strip, the flexible
        // central area, and — with a workspace open — an auto-height status bar.
        // The former egui top/central/bottom panels are gone; each band paints its
        // own surface inside its node.
        let full = ui.available_rect_before_wrap();
        let avail_w = full.width().max(0.0);
        let mut central_rect: Option<egui::Rect> = None;
        let mut root_style = tui::column();
        tui::fill(&mut root_style);
        tui::tui(ui, ui.id().with("app_root"))
            .reserve_available_space()
            .style(root_style)
            .show(|b| {
                // --- title strip (fixed 28pt band) ---
                b.style(tui::fixed_main(TITLE_BAR_H)).add(|tree| {
                    let state = CommandState {
                        session: &self.session,
                        layout: &self.layout,
                        collab: &self.collab,
                        has_root: title_has_root,
                    };
                    menu = self.ui.title_bar(
                        tree,
                        TitleBarInput {
                            has_root: title_has_root,
                            settings: &self.settings,
                            bindings: &self.bindings,
                            state: &state,
                            has_document,
                            exporting,
                        },
                    );
                });

                if let Some(format) = menu.export {
                    self.start_export(format, ctx.clone());
                }
                if menu.open_png_dialog {
                    self.export.open_dialog();
                }
                if let Some(path) = menu.open_recent {
                    self.open_recent(path);
                }
                if menu.clear_recent {
                    self.settings.workspace.recent_projects.clear();
                    self.mark_settings_dirty();
                }
                if let Some(id) = menu.command {
                    self.run_command(id, &ctx);
                }
                if menu.typst_help {
                    self.ui.typst_help_open = true;
                }

                if self.ui.landing || self.tree.root.is_none() {
                    // Popped windows only exist in the workspace view; forgetting them on
                    // the way to the landing page avoids a stale flag on the next open.
                    self.layout.popped = [false; 3];
                    self.frame_tree.hover = None;
                    let landing_has_root = self.tree.root.is_some();
                    let mut landing_out = None;
                    b.style(tui::flexible())
                        .id(egui::Id::new("typ_edit_landing_band"))
                        .add(|tree| {
                            landing_out =
                                Some(self.ui.landing_pane(tree, landing_has_root, &self.settings));
                        });
                    let landing = landing_out.expect("landing content ran");
                    if landing.open_file {
                        self.pick_and_open_file();
                    }
                    if landing.open_folder {
                        self.pick_and_open_folder();
                    }
                    if landing.new_project {
                        self.new_project.open_fresh();
                    }
                    if landing.join_collab {
                        self.collab.open_join_dialog(&self.settings);
                    }
                    if let Some(path) = landing.recent.open {
                        self.open_recent(path);
                    } else if let Some(path) = landing.recent.remove {
                        remove_recent(&mut self.settings.workspace.recent_projects, &path);
                        self.mark_settings_dirty();
                    } else if landing.recent.clear {
                        self.settings.workspace.recent_projects.clear();
                        self.mark_settings_dirty();
                    }
                } else {
                    let mut panes = PaneOutputs {
                        tree: TreeAction::None,
                        editor: EditorOutput::default(),
                        editor_shown: false,
                        preview: PreviewAction::None,
                    };
                    let cursor_byte = self.compile.cursor_byte;
                    let (line, column) = self.session.tabs[self.session.active_tab]
                        .editor
                        .line_column(cursor_byte);
                    let info = StatusInfo {
                        words: self.compile.parse.stats.words,
                        chars: self.compile.parse.stats.chars.0,
                        pages: self.compile.page_count,
                        line,
                        column,
                        cursors: self.session.tabs[self.session.active_tab]
                            .editor
                            .cursor_count(),
                        compile: if self.downloads.fetching() {
                            crate::shell::status::CompileState::Fetching
                        } else {
                            self.compile.compile_state()
                        },
                        save_state: {
                            let tab = &self.session.tabs[self.session.active_tab];
                            let saving = self
                                .pending_save
                                .as_ref()
                                .is_some_and(|pending| pending.tab_id == tab.id);
                            crate::shell::status::save_state(
                                tab.dirty,
                                tab.editor.path.is_some(),
                                saving,
                            )
                        },
                        blame: self.git_ui.status_blame(
                            &self.git,
                            &self.session,
                            self.settings.git.git_enabled,
                            line,
                        ),
                        health_warning: self.ui.worker_degraded.as_ref().map(|pipeline| {
                            t_with("status.worker_degraded", &[("pipeline", pipeline)])
                        }),
                        git_warning: self
                            .git
                            .snapshot
                            .error
                            .as_ref()
                            .map(|error| t_with("status.git_unavailable", &[("err", error)])),
                        collab: self.collab.status_text(),
                        background_tasks: self.downloads.active_count(),
                    };
                    let mut collab_clicked = false;
                    let mut background_tasks_clicked = false;

                    // --- central area: the three-column workspace ---
                    // Three-column main area. The tree outlines the row of the open file.
                    self.tree.open_file = self.editor().path.clone();
                    b.style(tui::flexible_row())
                        .id(egui::Id::new("typ_edit_shell_columns"))
                        .add_with_background_ui(
                            |ui, container| {
                                let area = container.full_container();
                                central_rect = Some(area);
                                ui.painter().rect_filled(area, CornerRadius::same(0), board);
                            },
                            |tree, _| {
                                let mut layout = std::mem::take(&mut self.layout);
                                layout.compact = self.settings.appearance.compact_mode;
                                layout.add_panes(tree, avail_w, &mut |pane, tree| {
                                    self.render_pane(pane, tree, &mut panes)
                                });
                                self.layout = layout;
                                if self.layout.observe_frame_layout() {
                                    // A geometry change can land after taffy's sizing
                                    // pass; request one more frame so the shell cannot
                                    // sit on the previous pane rectangles until input.
                                    tree.egui_ctx().request_repaint();
                                }
                            },
                        );

                    // --- status bar: auto-height band at the bottom ---
                    // The band paints the board and the thin status card; its
                    // only child is a tui row holding the status items. Keeping
                    // the width/height policy (100% width, auto height) on the
                    // band means the padding lives on the row, not here.
                    let compact = self.settings.appearance.compact_mode;
                    let mut status_style = tui::column();
                    status_style.size = Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    };
                    b.style(status_style)
                        .id(egui::Id::new("typ_edit_status_band"))
                        .add_with_background_ui(
                            |ui, container| {
                                let band = container.full_container();
                                ui.painter().rect_filled(band, CornerRadius::same(0), board);
                                let (left, top, right, bottom) = if compact {
                                    (0.0, 0.0, 0.0, 0.0)
                                } else {
                                    (10.0, 5.0, 10.0, 10.0)
                                };
                                let card_rect = egui::Rect::from_min_max(
                                    egui::pos2(band.left() + left, band.top() + top),
                                    egui::pos2(band.right() - right, band.bottom() - bottom),
                                );
                                let theme = Theme::from_ui(ui);
                                let radius = if compact { 0 } else { theme.radii.status };
                                let painter = ui.painter();
                                painter.rect_filled(
                                    card_rect,
                                    CornerRadius::same(radius),
                                    theme.colors.card,
                                );
                                painter.rect_stroke(
                                    card_rect,
                                    CornerRadius::same(radius),
                                    Stroke::new(theme.strokes.card, theme.colors.card_stroke),
                                    StrokeKind::Inside,
                                );
                            },
                            |tree, _| {
                                // Clicking the error indicator reveals the preview's error list;
                                // clicking the collaboration text opens the connection info.
                                let action = status::show(tree, &info, compact);
                                if action.errors_clicked
                                    && self.layout.collapsed[Pane::Preview as usize]
                                {
                                    self.layout.collapsed[Pane::Preview as usize] = false;
                                }
                                collab_clicked = action.collab_clicked;
                                background_tasks_clicked = action.background_tasks_clicked;
                            },
                        );
                    if collab_clicked {
                        self.collab.open_info_dialog();
                    }
                    if background_tasks_clicked {
                        self.downloads.panel_open = true;
                    }

                    self.render_popped_panes(&ctx, &mut panes);
                    if !panes.editor_shown {
                        self.hover.reset_hover();
                    }

                    self.handle_tree_action(panes.tree);

                    self.handle_editor_output(
                        &ctx,
                        panes.editor_shown,
                        panes.editor,
                        panes.preview,
                    );
                }
            });

        // Splitter zones over the three-column band, registered after the tree so the
        // zones win overlapping hit-tests (the standard "resize handle on top" behaviour).
        // A drag is applied on the next frame; boundaries come from the pure taffy policy.
        if let Some(area) = central_rect {
            let mut layout = std::mem::take(&mut self.layout);
            layout.splitters(ui, area, avail_w);
            self.layout = layout;
        }

        self.frame_overlays(ui);
    }

    /// Renders the popped panes into their own immediate viewports.
    fn render_popped_panes(&mut self, ctx: &egui::Context, panes: &mut PaneOutputs) {
        // Popped panes render into their own OS windows (their main-window column
        // is a dock strip). Each child viewport is re-rendered synchronously here;
        // child input repaints the parent so this loop re-runs. Placed before the
        // action handling so a same-frame pop/dock lands without a one-frame delay.
        //
        // Each popped window consumes its own viewport's keys through the shared
        // shortcut pipeline below, so menu shortcuts work with a child focused.
        for pane in [Pane::Tree, Pane::Editor, Pane::Preview] {
            let i = pane as usize;
            if !self.layout.popped[i] {
                continue;
            }
            let close_requested = ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of(("shell_pane_viewport", i)),
                egui::ViewportBuilder::default()
                    .with_title(format!("{} — Typst Editor", t(PANE_TITLES[i])))
                    .with_inner_size([560.0, 720.0])
                    .with_min_inner_size([220.0, 160.0]),
                |vui, _class| {
                    // Popped editor/preview surfaces have their own viewport; the
                    // main-window guard cannot see their scrolling state.
                    if vui.input(|i| i.is_scrolling()) {
                        vui.ctx().request_repaint();
                    }
                    // OS drops are per-viewport: the root pass only sees the main
                    // window's drops, so the child window imports its own.
                    self.import_dropped_files(vui.ctx());
                    // Shortcuts must be consumed in the viewport that received the
                    // keys: the root pass never sees a child window's input. Same
                    // pipeline as the main window, so shortcuts keep working while
                    // a popped pane has focus. Whole-app commands must target the
                    // root viewport explicitly (see `Exit`).
                    for id in self.ui.handle_shortcuts(vui, &self.bindings) {
                        self.run_command(id, vui.ctx());
                    }
                    let hovered = vui.input(|i| i.raw.hovered_files.len());
                    // Same surface as a docked pane's card, not the
                    // near-black board backdrop.
                    vui.painter().rect_filled(
                        vui.max_rect(),
                        CornerRadius::same(0),
                        crate::theme::card(vui),
                    );
                    egui::Frame::new()
                        .inner_margin(egui::Margin::same(6))
                        .show(vui, |ui| {
                            // A popped pane is its own window: a fresh
                            // top-level tui tree is legal here (it is a
                            // plain Ui host, not a child of the main
                            // window's tree).
                            let mut style = tui::column();
                            tui::fill(&mut style);
                            tui::tui(ui, ui.id().with(("typ_edit_popped_pane", i)))
                                .reserve_available_space()
                                .style(style)
                                .show(|tree| self.render_pane(pane, tree, panes));
                        });
                    if hovered > 0 {
                        show_drop_hint(vui.ctx(), hovered);
                    }
                    vui.input(|i| i.viewport().close_requested())
                },
            );
            if close_requested {
                self.layout.popped[i] = false;
            }
        }
    }

    /// Renders settings, dialogs, overlays and the final window-chrome pass.
    fn frame_overlays(&mut self, ui: &mut egui::Ui) {
        let font_catalog = self.fonts.current();
        let settings_outcome =
            self.ui
                .settings_window_ui(ui, &mut self.settings, &font_catalog, &mut self.gallery);
        for request in settings_outcome.requests {
            self.gallery.apply_package_request(request, ui.ctx());
        }
        if settings_outcome.changed {
            // Settings are rendered after the pane tree, so a change that can
            // affect geometry (frame-tree dock, fonts, scale, theme) only
            // lands in the next frame. Without an explicit repaint an idle app
            // can sit on the half-laid-out frame until the pointer moves.
            ui.ctx().request_repaint();
            crate::i18n::apply_language(&self.settings);
            let catalog = self.fonts.current();
            crate::ui_fonts::install_egui_fonts(
                ui.ctx(),
                &self.settings.fonts.ui_font,
                &self.settings.fonts.editor_font,
                &catalog,
            );
            self.invalidate_editor_font_layout();
            ui.ctx()
                .set_theme(crate::theme::preference(self.settings.appearance.theme));
            ui.ctx().set_zoom_factor(crate::ui_fonts::ui_zoom_factor(
                self.settings.appearance.ui_scale,
            ));
            ui.ctx().all_styles_mut(|style| {
                crate::theme::install_text_styles(style, self.settings.fonts.ui_font_size);
            });
            self.rebuild_bindings();
            self.mark_settings_dirty();
            if settings_outcome.git_enabled_changed {
                self.git
                    .refresh(self.settings.git.git_enabled, self.tree.root.clone());
            }
        }

        if self.ui.shortcuts_open {
            self.ui.shortcuts_window(ui.ctx(), &self.bindings);
        }
        if self.ui.typst_help_open {
            self.ui.typst_help_window(ui.ctx());
        }

        self.git_ui.git_diff_window(ui.ctx(), &self.git);

        // New file/folder prompt and delete confirmation.
        let tree_outcome = self
            .session
            .tree_dialog_ui(ui.ctx(), &mut self.ui, &mut self.tree);
        if let Some(path) = tree_outcome.open_file {
            self.open_file(path);
        }
        if let Some((old, new)) = tree_outcome.rename {
            self.apply_rename(&old, &new);
        }
        if let Some(deleted) = tree_outcome.delete {
            if deleted.main_deleted {
                if let Err(err) = self.write_main_marker() {
                    log::warn!("cannot clear workspace marker: {err}");
                }
                self.trigger_compile();
            }
            self.session.clear_deleted_paths(&deleted.path);
            self.recovery.note_change();
        }
        // Unsaved-changes prompt for a tab or the whole application.
        match self.session.close_prompt_ui(ui.ctx()) {
            ClosePromptOutcome::None => {}
            ClosePromptOutcome::SaveTab(index) => {
                if self.save_tab(index) {
                    self.remove_tab(index);
                } else {
                    self.session.prompt_close(ClosePrompt::Tab(index));
                }
            }
            ClosePromptOutcome::RemoveTab(index) => self.remove_tab(index),
            ClosePromptOutcome::SaveAllClose => {
                if self.save_all_dirty() {
                    self.session.allow_close_now();
                    self.release_project_lock();
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    self.session.prompt_close(ClosePrompt::App);
                }
            }
            ClosePromptOutcome::CloseApp => {
                self.session.allow_close_now();
                self.release_project_lock();
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        // The active tab's disk-vs-memory conflict sits above ordinary dialogs;
        // inactive tabs keep their conflict queued until they are activated.
        self.external_change_dialog(ui.ctx());
        // Crash-recovery snapshot restore question.
        if let Some(choice) = self.recovery.prompt_ui(ui.ctx()) {
            let outcome = self.recovery.resolve_choice(choice);
            self.apply_recovery_outcome(outcome);
        }
        // Inline rename overlay / label confirmation.
        let root = self.tree.root.clone();
        if let Some(edits) = self
            .ide
            .rename_ui(ui.ctx(), &self.engine, &self.compile, root)
        {
            self.apply_symbol_rename(edits);
        }

        let export_ctx = ui.ctx().clone();
        match self.export.dialog(ui, &mut self.settings) {
            ExportDialogAction::ExportPng => {
                self.mark_settings_dirty();
                self.start_export(ExportFormat::Png, export_ctx);
                self.export.close_dialog();
            }
            ExportDialogAction::Close => self.export.close_dialog(),
            ExportDialogAction::None => {}
        }
        let new_project = self.new_project.ui(ui);
        if let Some(root) = new_project.created_root {
            self.open_root(root);
        }
        if let Some(toast) = new_project.toast {
            self.toast_error(toast);
        }
        if new_project.open_gallery {
            self.gallery.open_window(ui.ctx());
        }
        // The gallery floats above the new-project dialog it can be launched from.
        if let Some(GalleryAction::UseTemplate { name, version }) = self.gallery.ui(ui) {
            self.new_project
                .preset(&name, format!("@preview/{name}:{version}"));
        }

        // Collaboration dialogs sit below the palette (which floats above them).
        self.collab_dialogs(ui);

        // The palette floats above every dialog, so it renders last.
        let palette_choice = {
            let state = CommandState {
                session: &self.session,
                layout: &self.layout,
                collab: &self.collab,
                has_root: self.tree.root.is_some(),
            };
            self.ui.command_palette(ui, &self.bindings, &state)
        };
        if let Some(id) = palette_choice {
            self.run_command(id, ui.ctx());
        }

        if let super::downloads::DownloadAction::Confirm(path) = self.downloads.ui(ui.ctx()) {
            self.collab.get_chunked_file(&path, true);
        }

        // Persist pane geometry on change.
        if self.layout.widths != self.settings.layout.pane_widths
            || self.layout.collapsed != self.settings.layout.pane_collapsed
        {
            self.settings.layout.pane_widths = self.layout.widths;
            self.settings.layout.pane_collapsed = self.layout.collapsed;
            self.mark_settings_dirty();
        }

        let hovered = ui.ctx().input(|i| i.raw.hovered_files.len());
        if hovered > 0 {
            show_drop_hint(ui.ctx(), hovered);
        }

        self.show_toast(ui);

        // Frameless windows have no OS resize borders; drive them ourselves.
        self.ui.window_resize_edges(ui.ctx());
    }

    /// Renders one pane into the window's tui tree, recording its result on
    /// `out`. Shared by the docked main-window pass and each popped pane's
    /// immediate viewport (which owns a fresh top-level tree).
    fn render_pane(&mut self, pane: Pane, tree: &mut Tui, out: &mut PaneOutputs) {
        let id = egui::Id::new(("typ_edit_pane_body", pane as usize));
        tree.style(tui::flexible_column())
            .id(id)
            .add(|tree| match pane {
                Pane::Tree => out.tree = self.left_pane(tree),
                Pane::Editor => {
                    out.editor_shown = true;
                    out.editor = self.editor_pane(tree);
                }
                Pane::Preview => out.preview = self.preview_pane_body(tree),
            });
    }

    /// Renders the preview column body (with the debug frame-tree inspector
    /// docked to its right when enabled) and returns the preview action the
    /// caller must apply.
    fn preview_pane_body(&mut self, tree: &mut Tui) -> PreviewAction {
        // The inspector docks to the right only with a live document and enough
        // width to stay useful; otherwise the preview keeps the full pane.
        let dock = self.settings.debug.frame_tree.enabled
            && self.compile.doc.is_some()
            && tree.egui_ui().max_rect().width() >= 420.0;
        if self.frame_tree.docked != dock {
            self.frame_tree.docked = dock;
            // Do not leave the one-frame sizing-pass width on screen until the
            // next mouse move; the dock's nested Panel must get another frame.
            tree.egui_ctx().request_repaint();
        }
        if dock {
            let path = self.session.tabs[self.session.active_tab]
                .editor
                .path
                .clone();
            let selection = self.compile.selection.clone();
            let cursor_byte = self.compile.cursor_byte;
            let id = egui::Id::new("typ_edit_preview_docked");
            tree.style(tui::flexible_column()).id(id).ui(|ui| {
                let out = self.frame_tree.pane(
                    ui,
                    &self.compile,
                    &self.settings.debug.frame_tree,
                    self.tree.root.as_deref(),
                    path.as_deref(),
                    &selection,
                    cursor_byte,
                );
                let FrameTreeOutcome { jump, hover } = out;
                if let Some((path, byte)) = jump {
                    if self.editor().path.as_deref() != Some(path.as_path()) {
                        self.open_file(path);
                    }
                    self.editor_mut().jump_to_byte(byte);
                    self.compile.editor_gate.request_focus();
                }
                self.frame_tree.hover = hover;
                // Feed the (same-frame) hover to the preview before it paints;
                // the editor pane reads the stored hover next frame.
                let highlights: Vec<DebugHighlight> = self
                    .frame_tree
                    .hover
                    .iter()
                    .map(|hover| DebugHighlight {
                        page: hover.page,
                        left_pt: hover.rect.min.x.to_pt() as f32,
                        top_pt: hover.rect.min.y.to_pt() as f32,
                        right_pt: hover.rect.max.x.to_pt() as f32,
                        bottom_pt: hover.rect.max.y.to_pt() as f32,
                    })
                    .collect();
                self.preview.state.set_debug_highlights(highlights);

                let out = self.preview.preview_docked_ui(
                    ui,
                    &self.compile,
                    &self.settings,
                    &self.ui.error_rows,
                );
                if let Some(dir) = out.open_root {
                    self.open_root(dir);
                }
                if let Some(msg) = out.toast {
                    self.toast_warning(msg);
                }
                if !matches!(out.bar_action, PreviewAction::None) {
                    self.handle_preview_action(out.bar_action);
                }
                out.canvas_action
            })
        } else {
            // No tree rendered this frame: never leave a stale highlight.
            self.frame_tree.hover = None;
            self.preview.state.set_debug_highlights(Vec::new());
            let out = self.preview.preview_pane(
                tree,
                &self.compile,
                &self.settings,
                self.tree.root.as_deref(),
                &self.ui.error_rows,
            );
            if let Some(dir) = out.open_root {
                self.open_root(dir);
            }
            if let Some(msg) = out.toast {
                self.toast_warning(msg);
            }
            if !matches!(out.bar_action, PreviewAction::None) {
                self.handle_preview_action(out.bar_action);
            }
            out.canvas_action
        }
    }
}

impl eframe::App for TypEditApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(target_os = "macos")]
        super::macos_window::sync_rounding(&*_frame, ctx);
        self.frame_logic(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame_ui(ui);
    }

    fn on_exit(&mut self) {
        self.release_project_lock();
        // Flush any settings changed by the final frame before teardown; a
        // failure here is logged inside the persist and cannot be surfaced.
        let _ = self.persist_settings(true);
        // Same for coalesced remote-edit mirror writes.
        self.flush_mirror_writes(true);
        // Stop the workers even before the recovery early-return below, so an
        // unresolved-recovery exit still joins the engine's threads.
        self.shutdown_background();
        // A graceful exit means save-all or discard already resolved the
        // buffers; a hard kill never reaches here, leaving the snapshot behind.
        if self.recovery.unresolved() {
            return;
        }
        self.recovery.shutdown_writer();
        if let Some(path) = self.recovery.take_snapshot_path()
            && let Err(err) = crate::platform::fs_ops::remove_file(&path)
        {
            log::warn!("cannot remove recovery snapshot: {err}");
        }
    }
}
