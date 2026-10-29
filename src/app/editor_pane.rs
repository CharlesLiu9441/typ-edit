use super::TypEditApp;
use super::state::SessionState;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use typ_edit_editor::{EditorInput, EditorOutput, SpellAction};
use typ_edit_i18n::{t, t_with};
use typ_edit_preview::PreviewAction;
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::tui::{self, Tui, TuiBuilderLogic};

/// Pointer dwell on one glyph before the semantic hover tooltip appears.
pub(super) const HOVER_DWELL: Duration = Duration::from_millis(300);
/// Pointer drift (points) from the dwell anchor that restarts the wait.
pub(super) const HOVER_MOVE_TOLERANCE: f32 = 3.0;

/// Whether the semantic hover tooltip may show: the pointer must have dwelled
/// on one glyph, stayed within the move tolerance, and not been dismissed.
pub(super) fn hover_due(since: Duration, moved: f32, suppressed: bool) -> bool {
    !suppressed && since >= HOVER_DWELL && moved <= HOVER_MOVE_TOLERANCE
}

/// Whether the only open tab is a pristine, unnamed scratch buffer.
pub(super) fn blank_scratch(session: &SessionState) -> bool {
    session.tabs.len() == 1
        && session.tabs[0].editor.path.is_none()
        && session.tabs[0].editor.is_empty()
        && !session.tabs[0].dirty
}

impl TypEditApp {
    /// Whether the only open tab is a pristine, unnamed scratch buffer.
    pub(super) fn blank_scratch(&self) -> bool {
        blank_scratch(&self.session)
    }

    /// Whether any modal/foreground editor-covering surface is open. While one
    /// is, the editor must not resolve hover bytes or keep a tooltip alive
    /// behind it: the pointer belongs to the modal, not the text underneath.
    fn modal_covers_editor(&self) -> bool {
        self.ui.settings_open
            || self.ui.shortcuts_open
            || self.ui.typst_help_open
            || self.ui.palette_open
            || !matches!(self.ui.tree_dialog, super::state::TreeDialog::None)
            || self.session.close_prompt.is_some()
            || self.recovery.prompt.is_some()
            || self.export.png_dialog
            || self.new_project.open
            || self.gallery.open
            || self.collab.modal_open()
            || self.downloads.modal_open()
            || self.git_ui.diff.is_some()
            || self.ide.rename.is_some()
            || self
                .external_changes
                .contains_key(&self.session.tabs[self.session.active_tab].id)
    }

    pub(super) fn editor_pane(&mut self, tree: &mut Tui) -> EditorOutput {
        let id = egui::Id::new("typ_edit_editor_pane");
        if self.blank_scratch() {
            let root = self.tree.root.clone();
            let main = root.as_deref().and_then(|root| {
                crate::workspace::main_file(root).or_else(|| {
                    let candidate = root.join("main.typ");
                    candidate.is_file().then_some(candidate)
                })
            });
            // Centered empty state: one tui column, buttons in a tui row.
            tree.style(tui::centered_column())
                .id(id.with("empty"))
                .add(|tree| {
                    tree.style(tui::label_leaf())
                        .id(id.with("empty_hint"))
                        .wrap_mode(egui::TextWrapMode::Extend)
                        .ui(|ui| {
                            let key = if root.is_some() {
                                "editor.choose_from_tree"
                            } else {
                                "editor.open_to_edit"
                            };
                            ui.label(
                                egui::RichText::new(t(key)).color(crate::theme::text_muted(ui)),
                            );
                        });
                    tree.style(tui::fixed(12.0))
                        .id(id.with("empty_gap"))
                        .add_empty();
                    tree.style(tui::row_gap(8.0))
                        .id(id.with("empty_buttons"))
                        .add(|tree| {
                            if let Some(main) = main.clone() {
                                tree.id(id.with("open_main")).ui(|ui| {
                                    if button(ui, &t("editor.open_main"), ButtonVariant::Cta, true)
                                        .clicked()
                                    {
                                        self.open_file(main.clone());
                                    }
                                });
                                tree.id(id.with("choose_file")).ui(|ui| {
                                    if button(
                                        ui,
                                        &t("editor.choose_file"),
                                        ButtonVariant::Secondary,
                                        true,
                                    )
                                    .clicked()
                                    {
                                        self.pick_and_open_file();
                                    }
                                });
                            } else {
                                tree.id(id.with("open_file")).ui(|ui| {
                                    if button(ui, &t("editor.open_file"), ButtonVariant::Cta, true)
                                        .clicked()
                                    {
                                        self.pick_and_open_file();
                                    }
                                });
                                if root.is_none() {
                                    tree.id(id.with("open_folder")).ui(|ui| {
                                        if button(
                                            ui,
                                            &t("editor.open_folder"),
                                            ButtonVariant::Cta,
                                            true,
                                        )
                                        .clicked()
                                        {
                                            self.pick_and_open_folder();
                                        }
                                    });
                                }
                            }
                        });
                });
            return EditorOutput::default();
        }
        // The tab strip's container slot; the widget keeps its own internal
        // layout and reports its height.
        self.tab_bar(tree);
        if !self.ui.fonts_ready {
            // Do not shape the document with the fallback font: the editor's
            // per-line galley cache would keep those first-screen galleys after
            // the configured font is installed (later scrolled lines would use
            // the new font, producing a mixed-font first screen).
            tree.style(tui::centered_column())
                .id(id.with("fonts_loading"))
                .add(|tree| {
                    tree.style(tui::label_leaf())
                        .id(id.with("fonts_loading_hint"))
                        .wrap_mode(egui::TextWrapMode::Extend)
                        .ui(|ui| {
                            ui.label(
                                egui::RichText::new(t("editor.loading_fonts"))
                                    .color(crate::theme::text_muted(ui)),
                            );
                        });
                });
            return EditorOutput::default();
        }
        if self.compile.editor_gate.is_pending(
            self.session.tabs[self.session.active_tab]
                .editor
                .len_bytes(),
        ) {
            tree.style(tui::centered_column())
                .id(id.with("opening"))
                .add(|tree| {
                    tree.style(tui::label_leaf())
                        .id(id.with("opening_hint"))
                        .wrap_mode(egui::TextWrapMode::Extend)
                        .ui(|ui| {
                            ui.label(
                                egui::RichText::new(t("editor.opening"))
                                    .color(crate::theme::text_muted(ui)),
                            );
                        });
                });
            return EditorOutput::default();
        }
        let request_focus = self.compile.editor_gate.take_focus();
        if request_focus && self.layout.popped[crate::shell::layout::Pane::Editor as usize] {
            // egui focus is Context-local, but keystrokes go to the OS-focused
            // window: a programmatic jump into a popped editor must also raise it.
            tree.egui_ctx()
                .send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        tree.style(tui::flexible()).id(id.with("editor")).ui(|ui| {
            let plain_text = self.active_plain_text();
            // Inspector hover: last frame's hovered node span, only when it belongs to
            // the active file (the tree renders after the editor column; hover changes
            // request a repaint).
            let debug_span: Option<std::ops::Range<usize>> = self
                .settings
                .debug
                .frame_tree
                .enabled
                .then_some(self.frame_tree.hover.as_ref())
                .flatten()
                .and_then(|hover| {
                    let (path, range) = hover.source.as_ref()?;
                    (self.editor().path.as_deref() == Some(path.as_path())).then(|| range.clone())
                });
            let debug_highlight_color = {
                let [r, g, b, a] = self.settings.debug.frame_tree.highlight_color;
                egui::Color32::from_rgba_unmultiplied(r, g, b, a.max(24))
            };
            let collab_history = self.collab.is_active();
            let active_rel = self.editor().path.as_ref().and_then(|path| {
                self.tree
                    .root
                    .as_ref()
                    .and_then(|root| path.strip_prefix(root).ok())
                    .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            });
            let remote_carets = active_rel
                .as_deref()
                .map_or_else(Vec::new, |rel| self.collab.remote_carets_for(rel));
            let input = EditorInput {
                highlights: &self.compile.parse.highlights,
                diagnostics: &self.compile.merged_diags,
                spell_issues: &self.spell.issues,
                debug_highlights: debug_span.as_slice(),
                debug_highlight_color,
                blocked_ranges: &self.compile.parse.layout.blocked_ranges,
                code_ranges: &self.compile.parse.layout.code_ranges,
                math_ranges: &self.compile.parse.layout.math_ranges,
                math_atoms: &self.compile.parse.layout.math_atoms,
                folds: &self.compile.parse.folds,
                list_lines: &self.compile.parse.layout.list_lines,
                completions: &self.hover.completions,
                definitions: &self.compile.parse.ide.definitions,
                world_completions: match &self.hover.world_completions {
                    Some((_, items)) => items.as_slice(),
                    None => &[],
                },
                completion_max: self.settings.editor.completion_max,
                parse_generation: self.compile.parse_generation,
                call_params: self.compile.parse.ide.call_params.as_ref(),
                font_family: crate::ui_fonts::EDITOR_FAMILY,
                font_size: self.settings.fonts.editor_font_size,
                wrap_mode: self.settings.editor.wrap.to_mode(),
                line_spacing: self.settings.editor.line_spacing,
                wrap_line_spacing: self.settings.editor.wrap_line_spacing,
                plain_text,
                request_focus,
                blame_inline: self.git_ui.blame_inline_for_caret(
                    &self.git,
                    &self.session,
                    self.compile.cursor_byte,
                    self.settings.git.git_enabled,
                    self.settings.git.git_blame_inline,
                ),
                collab_history,
                remote_carets: &remote_carets,
            };
            // Direct indexing (not `editor_mut`): `input` already borrows other
            // fields of `self`, which a `&mut self` helper would conflict with.
            let mut output = self.session.tabs[self.session.active_tab]
                .editor
                .show(ui, input);
            if let Some(request) = output.history_request {
                let op = match request {
                    typ_edit_editor::HistoryRequest::Undo => typ_edit_collab::HistoryOp::Undo,
                    typ_edit_editor::HistoryRequest::Redo => typ_edit_collab::HistoryOp::Redo,
                };
                self.collab.request_history(op);
            }
            self.handle_editor_drop(ui, output.pointer_byte.map(|byte| byte.0));
            let hover_path = self.session.tabs[self.session.active_tab]
                .editor
                .path
                .clone();
            if self.modal_covers_editor() {
                // Do not merely hide the popup: clear the dwell anchor too, so
                // closing the modal does not instantly resurrect a stale tip.
                self.hover.reset_hover();
            } else {
                self.hover.show_hover_popup(
                    ui,
                    output.hover_byte.map(|byte| byte.0),
                    &self.compile.merged_diags,
                    &self.compile,
                    hover_path.as_deref(),
                );
            }
            if let Some(image) = output.paste_image.take() {
                self.paste_image(image);
            }
            output
        })
    }

    /// Imports a `PathBuf` DnD payload dropped on the editor at `pointer_byte`.
    ///
    /// Runs after the editor is shown, so `pointer_byte` is this frame's release
    /// position. The payload is consumed exactly once; a release anywhere else
    /// leaves it to egui's automatic end-of-frame cleanup.
    fn handle_editor_drop(&mut self, ui: &egui::Ui, pointer_byte: Option<usize>) {
        let ctx = ui.ctx();
        let Some(byte) = pointer_byte else {
            return;
        };
        if !egui::DragAndDrop::has_payload_of_type::<PathBuf>(ctx) {
            return;
        }
        ui.painter().rect_stroke(
            ui.max_rect(),
            egui::CornerRadius::same(2),
            egui::Stroke::new(2.0, ui.visuals().hyperlink_color),
            egui::StrokeKind::Inside,
        );
        if ctx.input(|i| i.pointer.any_released())
            && let Some(path) = egui::DragAndDrop::take_payload::<PathBuf>(ctx)
        {
            self.import_image_sources(&[path.as_ref().clone()], Some(byte));
        }
    }

    /// Applies the editor frame's output (cursor, dirty flag, jumps) and the preview action.
    pub(super) fn handle_editor_output(
        &mut self,
        ctx: &egui::Context,
        editor_shown: bool,
        editor_out: EditorOutput,
        preview_action: PreviewAction,
    ) {
        // Collab: feed the active editor's carets to the controller (throttled send).
        if self.collab.is_active()
            && let (Some(path), Some(root)) = (self.editor().path.clone(), self.tree.root.clone())
            && let Ok(rel) = path.strip_prefix(&root)
        {
            let file = rel.to_string_lossy().replace('\\', "/");
            let carets: Vec<u64> = editor_out
                .selections
                .iter()
                .map(|range| range.start.0 as u64)
                .collect();
            let selections: Vec<(u64, u64)> = editor_out
                .selections
                .iter()
                .map(|range| (range.start.0 as u64, range.end.0 as u64))
                .collect();
            self.collab.feed_local_cursors(file, carets, selections);
        }
        self.compile
            .set_pointer_byte(editor_out.pointer_byte.map(|byte| byte.0));
        if editor_shown {
            let previous = self.compile.cursor_byte;
            self.compile.set_cursor_byte(editor_out.cursor_byte.0);
            self.compile.set_selection(
                editor_out
                    .selections
                    .iter()
                    .map(|range| range.start.0..range.end.0)
                    .collect(),
            );
            if self.compile.cursor_byte != previous
                && self.settings.preview.auto_scroll_preview
                && !self.active_plain_text()
            {
                let byte = self.compile.cursor_byte;
                self.preview.arm_follow(byte, Instant::now());
            }
        }
        if editor_out.changed {
            self.session.tabs[self.session.active_tab].dirty = true;
            self.recovery.note_change();
            self.trigger_parse();
            // Member candidates describe the pre-edit caret; drop them and let
            // the next `.` re-request.
            self.hover.clear_world_completions();
            self.ide.clear_completions_pending();
        }
        if let Some(byte) = editor_out.completion_request {
            let plain = self.active_plain_text();
            let root = self.tree.root.clone();
            self.ide.request_completions(
                byte.0,
                &self.engine,
                &self.compile,
                &self.session,
                root,
                plain,
            );
        }
        // The stored list is only valid at the caret byte it was fetched for; a
        // caret relocation (arrows, a click) must not keep painting it.
        if self
            .hover
            .world_completions
            .as_ref()
            .is_some_and(|(byte, _)| {
                super::ide::completions_moved_away(*byte, editor_out.cursor_byte.0)
            })
        {
            self.hover.clear_world_completions();
            self.ide.clear_completions_pending();
        }
        if let Some(offset) = editor_out.show_in_preview {
            let path = self.editor().path.clone();
            self.preview
                .reveal_cursor_in_preview(&self.compile, path.as_deref(), offset.0);
        }
        if let Some(byte) = editor_out.ctrl_click {
            self.jump_to_definition(byte.0);
        }
        if let Some(byte) = editor_out.rename_request {
            let anchor = editor_out
                .caret_screen
                .map(|rect| egui::pos2(rect.left(), rect.bottom()));
            let plain = self.active_plain_text();
            let path = self.editor().path.clone();
            let root = self.tree.root.clone();
            self.ide.request_rename(
                byte.0,
                anchor,
                &self.engine,
                &self.compile,
                super::ide::RenameTarget {
                    path,
                    root,
                    plain_text: plain,
                },
            );
        }
        if editor_out.references_request.is_some() {
            let plain = self.active_plain_text();
            let path = self.editor().path.clone();
            let root = self.tree.root.clone();
            let cursor = self.compile.cursor_byte;
            let editor = &self.session.tabs[self.session.active_tab].editor;
            if let Some(msg) = self.ide.request_references(
                &self.engine,
                &self.compile,
                path,
                root,
                cursor,
                editor,
                plain,
            ) {
                self.raise_toast(msg);
            }
        }
        match editor_out.spell_action {
            Some(SpellAction::AddToDictionary(word)) => match self.spell.add_to_dictionary(&word) {
                Ok(()) => self.toast_success(t("spell.added_to_dictionary")),
                Err(err) => {
                    log::warn!("cannot save spell dictionary: {err}");
                    self.toast_error(t_with(
                        "toast.spell_dictionary_failed",
                        &[("err", &err.to_string())],
                    ));
                }
            },
            Some(SpellAction::Ignore(word)) => self.spell.ignore_spell_word(&word),
            None => {}
        }
        self.handle_preview_action(preview_action);
        let path = self.editor().path.clone();
        let plain = self.active_plain_text();
        self.preview
            .update_cursor_marker(ctx, &self.compile, path.as_deref(), plain);
        self.preview
            .update_selection_masks(ctx, &self.compile, path.as_deref(), plain);
    }
}
