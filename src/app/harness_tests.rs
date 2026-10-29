//! Headless frame-level integration harness (architecture review P0).
//!
//! Drives the real [`TypEditApp`] through `egui::Context::run_ui` without an
//! `eframe::Frame`, a window or a GPU, exercising the whole open → edit →
//! parse → compile → tab-switch path against the real background engine. The
//! harness lives inside `crate::app` so it can assert on the app's private
//! session/compile state directly.

use super::TypEditApp;
use crate::settings::Settings;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use typ_edit_engine::fonts::{FontCatalog, Fonts};
use typ_edit_model::{BytePos, CharPos, FileMatches, ProjectSearchResult, SearchMatch, TextEdit};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);
static FRAME_CLOCK: AtomicU64 = AtomicU64::new(0);

/// Shared system-font catalog: scanning it takes seconds, so every app in the
/// test binary reuses one instance.
fn shared_fonts() -> Arc<Fonts> {
    static FONTS: OnceLock<Arc<Fonts>> = OnceLock::new();
    Arc::clone(FONTS.get_or_init(|| Fonts::ready(FontCatalog::new())))
}

/// Redirects every config-directory access to one process-unique temp dir so no
/// test reads or writes the real user config. Idempotent across tests.
fn init_config() {
    let dir = std::env::temp_dir().join(format!("typ-edit-harness-config-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    crate::settings::set_config_dir_override(dir);
}

/// A headless app with default settings and the shared font catalog.
fn harness_app(ctx: &egui::Context, open: Option<PathBuf>) -> TypEditApp {
    harness_app_with(ctx, Settings::default(), open)
}

/// A headless app with explicit settings and the shared font catalog.
fn harness_app_with(
    ctx: &egui::Context,
    mut settings: Settings,
    open: Option<PathBuf>,
) -> TypEditApp {
    init_config();
    // Keep the harness from autosaving recovery snapshots mid-test.
    settings.recovery.crash_recovery_enabled = false;
    let fonts = shared_fonts();
    // The editor widget paints with the custom `editor` family, so it must be
    // bound even though the harness skips the CreationContext setup.
    let catalog = fonts.current();
    crate::ui_fonts::install_egui_fonts(
        ctx,
        &settings.fonts.ui_font,
        &settings.fonts.editor_font,
        &catalog,
    );
    TypEditApp::new_with(ctx, None, fonts, settings, open)
}

/// A unique, self-cleaning scratch directory under the OS temp dir.
fn scratch_dir(tag: &str) -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "typ-edit-harness-{}-{seq}-{nanos}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Copies `test-project/` into a fresh scratch dir and returns its root.
fn copy_test_project(tag: &str) -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test-project");
    let dst = scratch_dir(tag);
    copy_dir(&src, &dst);
    dst
}

fn copy_dir(src: &Path, dst: &Path) {
    for entry in std::fs::read_dir(src).expect("read source dir") {
        let entry = entry.expect("dir entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            std::fs::create_dir_all(&to).expect("create subdir");
            copy_dir(&from, &to);
        } else {
            std::fs::copy(&from, &to).expect("copy file");
        }
    }
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// A 1280x800 viewport, the size the app's `NativeOptions` uses.
fn raw_input() -> egui::RawInput {
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0));
    let mut input = egui::RawInput::default();
    if let Some(info) = input.viewports.get_mut(&egui::ViewportId::ROOT) {
        info.inner_rect = Some(rect);
        info.outer_rect = Some(rect);
        info.monitor_size = Some(egui::vec2(1280.0, 800.0));
        info.focused = Some(true);
    }
    input.screen_rect = Some(rect);
    input.time = Some(FRAME_CLOCK.fetch_add(1, Ordering::Relaxed) as f64 / 60.0);
    input
}

/// Drives one full frame: `logic` then `ui`, exactly as eframe does.
fn drive_frame(ctx: &egui::Context, app: &mut TypEditApp) {
    let mut output = ctx.run_ui(raw_input(), |ui| {
        app.frame_logic(ui.ctx());
        app.frame_ui(ui);
    });
    // No real painter applies the font/texture deltas, and epaint's
    // `TexturesDelta::drop` asserts they were handled.
    output.textures_delta.clear();
}

fn drive_frames(ctx: &egui::Context, app: &mut TypEditApp, frames: usize) {
    for _ in 0..frames {
        drive_frame(ctx, app);
    }
}

fn frame_with_events(ctx: &egui::Context, app: &mut TypEditApp, events: Vec<egui::Event>) {
    let mut input = raw_input();
    input.events = events;
    let mut output = ctx.run_ui(input, |ui| {
        app.frame_logic(ui.ctx());
        app.frame_ui(ui);
    });
    output.textures_delta.clear();
}

fn click_at(ctx: &egui::Context, app: &mut TypEditApp, pos: egui::Pos2) {
    for pressed in [true, false] {
        frame_with_events(
            ctx,
            app,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}

fn type_text(ctx: &egui::Context, app: &mut TypEditApp, text: &str) {
    for ch in text.chars() {
        frame_with_events(ctx, app, vec![egui::Event::Text(ch.to_string())]);
    }
}

/// Drives frames until `cond` holds or `timeout` elapses, sleeping briefly
/// between frames so the background workers can make progress.
fn pump_until(
    ctx: &egui::Context,
    app: &mut TypEditApp,
    timeout: Duration,
    mut cond: impl FnMut(&TypEditApp) -> bool,
    what: &str,
) {
    let start = Instant::now();
    loop {
        drive_frame(ctx, app);
        if cond(app) {
            return;
        }
        if start.elapsed() >= timeout {
            panic!("{}", timeout_report(app, what, start.elapsed()));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn timeout_report(app: &TypEditApp, what: &str, waited: Duration) -> String {
    format!(
        "headless harness timed out after {waited:?} waiting for {what}: \
         parse_in_flight={}, parse_generation={}, edit_generation={}, \
         compile_in_flight={}, has_doc={}, page_count={}, tabs={}, active_tab={}, \
         root={:?}",
        app.compile.parse_in_flight,
        app.compile.parse_generation,
        app.compile.edit_generation,
        app.compile.compile_in_flight,
        app.compile.doc.is_some(),
        app.compile.page_count,
        app.session.tabs.len(),
        app.session.active_tab,
        app.tree.root,
    )
}

/// A full parse result has landed for the active tab.
fn parse_settled(app: &TypEditApp) -> bool {
    !app.compile.parse_in_flight && app.compile.parse_generation > 0
}

const SETTLE: Duration = Duration::from_secs(15);

#[test]
fn harness_opens_workspace_and_drives_frames() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    drive_frames(&ctx, &mut app, 3);
    assert!(app.tree.root.is_none(), "no workspace before open_root");
    assert!(!app.ui.landing, "the manual home flag starts off");

    let root = copy_test_project("workspace");
    app.open_root(root.clone());
    drive_frames(&ctx, &mut app, 5);

    let main = root.join("main.typ");
    assert_eq!(app.tree.root.as_deref(), Some(root.as_path()));
    assert!(
        !app.ui.landing,
        "adopting a workspace leaves the landing page"
    );
    assert_eq!(app.tree.main_file.as_deref(), Some(main.as_path()));
    assert_eq!(
        app.session.tabs.len(),
        1,
        "the empty scratch slot is reused"
    );
    assert_eq!(app.editor().path.as_deref(), Some(main.as_path()));
    assert!(
        !app.editor().shared_text().is_empty(),
        "the main file's text was loaded"
    );

    app.shutdown_background();
    cleanup(&root);
}

#[test]
fn app_repaints_on_a_zero_delta_smoothing_frame() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = copy_test_project("scroll-repaint");
    app.open_root(root.clone());
    pump_until(
        &ctx,
        &mut app,
        SETTLE,
        parse_settled,
        "scroll repaint parse",
    );
    drive_frames(&ctx, &mut app, 3);

    let mut input = raw_input();
    input.events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::Vec2::ZERO,
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    let mut first = ctx.run_ui(input, |ui| {
        app.frame_logic(ui.ctx());
        app.frame_ui(ui);
    });
    first.textures_delta.clear();

    let mut second = ctx.run_ui(raw_input(), |ui| {
        app.frame_logic(ui.ctx());
        app.frame_ui(ui);
    });
    second.textures_delta.clear();
    let delay = second
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map_or(std::time::Duration::MAX, |viewport| viewport.repaint_delay);
    assert_eq!(
        delay,
        std::time::Duration::ZERO,
        "the app must keep repainting while egui reports an in-flight scroll"
    );

    app.shutdown_background();
    cleanup(&root);
}

#[test]
fn headless_trackpad_line_scroll_is_smooth() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = copy_test_project("trackpad-line");
    app.open_root(root.clone());
    pump_until(&ctx, &mut app, SETTLE, parse_settled, "trackpad line parse");
    drive_frames(&ctx, &mut app, 3);

    let text = (0..300)
        .map(|line| format!("line {line} with enough text to scroll\n"))
        .collect::<String>();
    app.editor_mut().set_text(text);
    app.trigger_parse();
    pump_until(&ctx, &mut app, SETTLE, parse_settled, "trackpad long text");
    drive_frames(&ctx, &mut app, 3);

    let pane = app.layout.frame_pane_rects[1];
    assert!(
        pane.width() > 100.0 && pane.height() > 100.0,
        "editor pane has a usable rect: {pane:?}"
    );
    let pointer = pane.center();
    let mut previous = {
        let (line, offset) = app.editor().debug_scroll_anchor();
        line as f32 * 20.0 + offset
    };
    let mut steps = Vec::new();
    let mut samples = Vec::new();
    for frame in 0..30 {
        let mut events = vec![egui::Event::PointerMoved(pointer)];
        if frame % 2 == 0 {
            events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -1.0 / 120.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let mut input = raw_input();
        input.time = Some(100.0 + frame as f64 / 60.0);
        input.events = events;
        let mut output = ctx.run_ui(input, |ui| {
            app.frame_logic(ui.ctx());
            app.frame_ui(ui);
        });
        output.textures_delta.clear();
        let (line, offset) = app.editor().debug_scroll_anchor();
        let now = line as f32 * 20.0 + offset;
        steps.push(now - previous);
        samples.push((line, offset));
        previous = now;
    }
    let steady = &steps[12..];
    assert!(
        steady.iter().all(|step| *step > 0.0),
        "headless trackpad stream produced a dead frame: steps={steps:?}, anchors={samples:?}, pane={pane:?}"
    );
    assert!(
        steady.windows(2).all(|pair| pair[1] >= pair[0] - 0.01),
        "the long tail must not drop to zero and restart: steps={steps:?}, anchors={samples:?}, pane={pane:?}"
    );

    app.shutdown_background();
    cleanup(&root);
}

#[test]
fn headless_trackpad_line_scrolls_preview_smoothly() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = copy_test_project("trackpad-preview");
    app.open_root(root.clone());
    pump_until(
        &ctx,
        &mut app,
        SETTLE,
        parse_settled,
        "trackpad preview parse",
    );
    pump_until(
        &ctx,
        &mut app,
        Duration::from_secs(30),
        |app| app.compile.doc.is_some(),
        "trackpad preview compile",
    );
    drive_frames(&ctx, &mut app, 3);

    let pane = app.layout.frame_pane_rects[2];
    assert!(
        pane.width() > 100.0 && pane.height() > 100.0,
        "preview pane has a usable rect: {pane:?}"
    );
    let pointer = pane.center();
    let mut previous = app.preview.state.debug_view_offset().y;
    let mut steps = Vec::new();
    for frame in 0..30 {
        let mut events = vec![egui::Event::PointerMoved(pointer)];
        if frame % 2 == 0 {
            events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -1.0 / 120.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let mut input = raw_input();
        input.time = Some(100.0 + frame as f64 / 60.0);
        input.events = events;
        let mut output = ctx.run_ui(input, |ui| {
            app.frame_logic(ui.ctx());
            app.frame_ui(ui);
        });
        output.textures_delta.clear();
        let now = app.preview.state.debug_view_offset().y;
        steps.push(now - previous);
        previous = now;
    }
    let steady = &steps[12..];
    assert!(
        steady.iter().all(|step| *step < 0.0),
        "preview trackpad stream produced a dead frame: steps={steps:?}, pane={pane:?}"
    );
    assert!(
        steady.windows(2).all(|pair| pair[1] <= pair[0] + 0.01),
        "the long tail must not drop to zero and restart: steps={steps:?}, pane={pane:?}"
    );

    app.shutdown_background();
    cleanup(&root);
}

#[test]
fn headless_trackpad_bursts_bridge_gaps() {
    fn run_case(delta: f32, burst: usize, period: usize) -> Vec<f32> {
        let ctx = egui::Context::default();
        let mut app = harness_app(&ctx, None);
        let root = copy_test_project("trackpad-burst");
        app.open_root(root.clone());
        pump_until(&ctx, &mut app, SETTLE, parse_settled, "burst parse");
        drive_frames(&ctx, &mut app, 2);
        let text = (0..300)
            .map(|line| format!("line {line} with enough text to scroll\n"))
            .collect::<String>();
        app.editor_mut().set_text(text);
        app.trigger_parse();
        drive_frames(&ctx, &mut app, 2);
        let pane = app.layout.frame_pane_rects[1];
        let pointer = pane.center();
        let mut previous = {
            let (line, offset) = app.editor().debug_scroll_anchor();
            line as f32 * 20.0 + offset
        };
        let mut steps = Vec::new();
        for frame in 0..60 {
            let mut events = vec![egui::Event::PointerMoved(pointer)];
            if frame % period < burst {
                events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, -delta),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            let mut input = raw_input();
            input.time = Some(100.0 + frame as f64 / 60.0);
            input.events = events;
            let mut output = ctx.run_ui(input, |ui| {
                app.frame_logic(ui.ctx());
                app.frame_ui(ui);
            });
            output.textures_delta.clear();
            let (line, offset) = app.editor().debug_scroll_anchor();
            let now = line as f32 * 20.0 + offset;
            steps.push(now - previous);
            previous = now;
        }
        app.shutdown_background();
        cleanup(&root);
        steps
    }

    let steps = run_case(1.0, 3, 10);
    assert!(
        steps[3..9].iter().all(|step| *step > 0.0),
        "the tail must bridge the gap instead of stopping and restarting: {steps:?}"
    );
    assert!(
        steps[9..].iter().any(|step| *step > 0.0),
        "the next event batch must continue the same motion: {steps:?}"
    );
}

#[test]
fn harness_applies_parse_after_a_programmatic_edit() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = copy_test_project("edit");
    app.open_root(root.clone());

    pump_until(&ctx, &mut app, SETTLE, parse_settled, "initial parse");
    let generation_before = app.compile.parse_generation;
    let text_before = app.editor().shared_text().to_string();

    let inserted = "= Injected heading\n";
    let edit = TextEdit {
        range: BytePos(0)..BytePos(0),
        new_text: inserted.to_owned(),
    };
    assert!(app.editor_mut().apply_byte_edits(&[edit]), "edit applies");
    app.trigger_parse();

    pump_until(
        &ctx,
        &mut app,
        SETTLE,
        |app| !app.compile.parse_in_flight && app.compile.parse_generation > generation_before,
        "parse after edit",
    );

    assert!(app.editor().shared_text().starts_with(inserted));
    assert_ne!(app.editor().shared_text().as_str(), text_before);
    assert!(
        app.compile.parse.stats.chars.0 > 0,
        "the applied parse carries refreshed text stats"
    );
    assert!(
        app.ui.error_rows.is_empty() || app.compile.compile_error.is_none(),
        "no fatal compile error while parsing the edit"
    );

    app.shutdown_background();
    cleanup(&root);
}

#[test]
fn harness_switches_tabs_and_keeps_editor_state() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = copy_test_project("tabs");
    app.open_root(root.clone());
    pump_until(&ctx, &mut app, SETTLE, parse_settled, "initial parse");

    let main = root.join("main.typ");
    let chapter = root.join("chapter.typ");
    app.open_file(chapter.clone());
    drive_frames(&ctx, &mut app, 3);
    pump_until(&ctx, &mut app, SETTLE, parse_settled, "chapter parse");

    assert_eq!(app.session.tabs.len(), 2);
    assert_eq!(app.session.active_tab, 1);
    assert_eq!(app.editor().path.as_deref(), Some(chapter.as_path()));
    assert!(
        app.editor().shared_text().contains("chapter"),
        "the chapter buffer is active"
    );

    app.activate_tab(0);
    drive_frames(&ctx, &mut app, 3);
    pump_until(
        &ctx,
        &mut app,
        SETTLE,
        parse_settled,
        "main parse after switch",
    );

    assert_eq!(app.session.active_tab, 0);
    assert_eq!(app.editor().path.as_deref(), Some(main.as_path()));
    assert!(
        app.editor().shared_text().contains("Typst Editor"),
        "the main buffer was restored on switch-back"
    );
    assert_eq!(
        app.session.tabs[1].editor.path.as_deref(),
        Some(chapter.as_path()),
        "the other tab kept its buffer"
    );
    assert!(app.session.tabs[1].editor.shared_text().contains("chapter"));

    app.shutdown_background();
    cleanup(&root);
}

#[test]
fn harness_compiles_an_open_document() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = copy_test_project("compile");
    app.open_root(root.clone());
    drive_frames(&ctx, &mut app, 3);

    let edit = TextEdit {
        range: BytePos(0)..BytePos(0),
        new_text: "= Compiled heading\n".to_owned(),
    };
    assert!(app.editor_mut().apply_byte_edits(&[edit]));
    app.trigger_parse();

    pump_until(
        &ctx,
        &mut app,
        Duration::from_secs(30),
        |app| app.compile.doc.is_some(),
        "compile document",
    );

    let doc = app.compile.doc.as_ref().expect("document present");
    assert!(
        app.compile.page_count >= 1,
        "the document has at least one page"
    );
    assert_eq!(
        doc.page_sizes.len(),
        app.compile.page_count,
        "page sizes match the page count"
    );
    assert!(doc.generation > 0);

    app.shutdown_background();
    cleanup(&root);
}

#[test]
fn harness_shutdown_stops_workers() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = copy_test_project("shutdown");
    app.open_root(root.clone());
    drive_frames(&ctx, &mut app, 5);

    let started = Instant::now();
    app.shutdown_background();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "shutdown must return within its deadline"
    );

    // Post-shutdown requests are no-ops: the frame must not panic.
    drive_frames(&ctx, &mut app, 1);

    cleanup(&root);
}

#[test]
fn harness_format_on_save_preserves_undo_history_and_marks_clean() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    app.settings.editor.format_on_save = true;
    let dir = scratch_dir("format-save");
    let path = dir.join("doc.typ");

    let index = app.session.active_tab;
    app.session.tabs[index]
        .editor
        .set_text("#let x=1\n".to_owned());
    let edit = TextEdit {
        range: BytePos(0)..BytePos(0),
        new_text: "#let y=2\n".to_owned(),
    };
    assert!(app.session.tabs[index].editor.apply_byte_edits(&[edit]));
    app.session.tabs[index].dirty = true;
    assert!(app.session.tabs[index].editor.can_undo());
    let before = app.session.tabs[index].editor.shared_text().to_string();

    assert!(app.write_tab(index, path.clone()), "save succeeds");

    let after = app.session.tabs[index].editor.shared_text().to_string();
    assert_ne!(after, before, "format-on-save rewrote the buffer");
    assert!(
        app.session.tabs[index].editor.can_undo(),
        "the undo history survives the format (the buffer was not rebuilt)"
    );
    assert!(!app.session.tabs[index].dirty, "a saved tab is clean");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read saved file"),
        after,
        "the file matches the formatted buffer"
    );

    app.shutdown_background();
    cleanup(&dir);
}

#[test]
fn harness_formatted_save_writes_only_after_the_format_reply() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    app.settings.editor.format_on_save = true;
    let dir = scratch_dir("format-save-wait");
    let path = dir.join("doc.typ");

    let index = app.session.active_tab;
    app.session.tabs[index]
        .editor
        .set_file(path.clone(), "#let x=1\n".to_owned());
    app.session.tabs[index].dirty = true;

    app.save_file();
    assert!(app.pending_save.is_some(), "the save waits for formatting");
    assert!(
        !path.exists(),
        "the file must not be written before the formatter answers"
    );
    assert_eq!(
        crate::shell::status::save_state(true, true, true),
        crate::shell::status::SaveState::Saving,
        "the status bar reports the formatter wait as saving"
    );

    let reply = app.format_worker.wait().expect("the format worker answers");
    app.apply_format_reply(reply);

    assert!(app.pending_save.is_none(), "the deferred save completed");
    assert!(!app.session.tabs[index].dirty, "the saved tab is clean");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read saved file"),
        "#let x = 1\n",
        "one Ctrl+S writes the formatted text"
    );

    app.shutdown_background();
    cleanup(&dir);
}

#[test]
fn harness_watch_batch_detects_disk_changes_and_respects_equal_content() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = scratch_dir("watch-external");
    let main = root.join("main.typ");
    std::fs::write(&main, "disk version\n").expect("write main");
    app.open_root(root.clone());

    let index = app.session.active_tab;
    let tab_id = app.session.tabs[index].id;
    let path = app.session.tabs[index]
        .editor
        .path
        .clone()
        .expect("main opened");
    app.session.tabs[index]
        .editor
        .set_text("memory version\n".to_owned());
    app.session.tabs[index].dirty = true;
    let compile_before = app.compile.compile_generation;

    app.apply_watch_paths(BTreeSet::from([path.clone()]));
    assert!(
        app.compile.compile_generation > compile_before,
        "a watcher batch recompiles just like an edit"
    );
    assert!(
        app.external_changes.contains_key(&tab_id),
        "a differing disk copy creates a tab-level conflict"
    );
    assert_eq!(
        app.session.tabs[index].editor.shared_text().as_str(),
        "memory version\n",
        "the buffer is untouched before the user chooses"
    );

    std::fs::write(&main, "memory version\n").expect("write equal copy");
    app.apply_watch_paths(BTreeSet::from([path.clone()]));
    assert!(
        !app.external_changes.contains_key(&tab_id),
        "equal disk bytes are the editor's own save and never prompt"
    );

    std::fs::write(&main, "new disk version\n").expect("write new copy");
    app.apply_watch_paths(BTreeSet::from([path]));
    assert!(app.external_changes.contains_key(&tab_id));
    app.resolve_external_change(tab_id, super::state::ExternalChangeChoice::UseDisk);
    assert_eq!(
        app.session.tabs[index].editor.shared_text().as_str(),
        "new disk version\n"
    );
    assert!(!app.session.tabs[index].dirty);
    assert!(!app.external_changes.contains_key(&tab_id));

    app.shutdown_background();
    cleanup(&root);
}

#[test]
fn harness_external_change_modal_accepts_the_disk_copy() {
    use super::ui_probe;

    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = scratch_dir("watch-modal");
    let main = root.join("main.typ");
    std::fs::write(&main, "disk version\n").expect("write main");
    app.open_root(root.clone());

    let index = app.session.active_tab;
    let tab_id = app.session.tabs[index].id;
    app.session.tabs[index]
        .editor
        .set_text("memory version\n".to_owned());
    app.session.tabs[index].dirty = true;
    app.apply_watch_paths(BTreeSet::from([main.clone()]));

    let other = root.join("other.typ");
    std::fs::write(&other, "other\n").expect("write other file");
    app.open_file(other);
    ui_probe::clear();
    drive_frames(&ctx, &mut app, 2);
    assert!(
        ui_probe::rect("external_change.use_disk").is_none(),
        "a conflict on an inactive tab does not render a dialog"
    );
    let main_index = app
        .session
        .tabs
        .iter()
        .position(|tab| tab.editor.path.as_deref() == Some(main.as_path()))
        .expect("the main tab is still open");
    app.activate_tab(main_index);
    ui_probe::clear();
    drive_frames(&ctx, &mut app, 2);
    let use_disk = ui_probe::rect("external_change.use_disk")
        .expect("the tab-level conflict renders its disk choice when active");
    click_at(&ctx, &mut app, use_disk.center());
    drive_frames(&ctx, &mut app, 2);

    assert!(
        !app.external_changes.contains_key(&tab_id),
        "choosing the disk copy clears the conflict"
    );
    assert_eq!(
        app.session.tabs[index].editor.shared_text().as_str(),
        "disk version\n"
    );
    assert!(!app.session.tabs[index].dirty);

    app.shutdown_background();
    cleanup(&root);
}

#[test]
fn harness_format_on_save_unchanged_text_adds_no_undo_entry() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    app.settings.editor.format_on_save = true;
    let dir = scratch_dir("format-save-noop");
    let path = dir.join("doc.typ");

    let index = app.session.active_tab;
    app.session.tabs[index]
        .editor
        .set_text("#let x = 1\n".to_owned());
    app.session.tabs[index].dirty = true;
    assert!(
        !app.session.tabs[index].editor.can_undo(),
        "a fresh load has no history"
    );

    assert!(app.write_tab(index, path.clone()), "save succeeds");

    assert!(
        !app.session.tabs[index].editor.can_undo(),
        "an unchanged format must not add an undo entry"
    );
    assert!(!app.session.tabs[index].dirty, "the saved tab is clean");

    app.shutdown_background();
    cleanup(&dir);
}

#[test]
fn harness_format_document_preserves_undo_history() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);

    let index = app.session.active_tab;
    app.session.tabs[index]
        .editor
        .set_text("#let x=1\n#let y=2\n".to_owned());
    let edit = TextEdit {
        range: BytePos(0)..BytePos(0),
        new_text: "#let z=3\n".to_owned(),
    };
    assert!(app.session.tabs[index].editor.apply_byte_edits(&[edit]));
    app.session.tabs[index].dirty = false;
    assert!(app.session.tabs[index].editor.can_undo());
    let before = app.session.tabs[index].editor.shared_text().to_string();

    app.format_document();
    let reply = app.format_worker.wait().expect("the format worker answers");
    app.apply_format_reply(reply);

    let after = app.session.tabs[index].editor.shared_text().to_string();
    assert_ne!(after, before, "format_document rewrote the buffer");
    assert!(after.contains("#let z = 3"), "operator spacing normalised");
    assert!(after.contains("#let x = 1"));
    assert!(
        app.session.tabs[index].editor.can_undo(),
        "the undo history survives the format"
    );
    assert!(
        app.session.tabs[index].dirty,
        "formatting marks the tab dirty"
    );

    app.shutdown_background();
}

#[test]
fn format_reply_is_dropped_when_the_buffer_changed_meanwhile() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let index = app.session.active_tab;
    app.session.tabs[index]
        .editor
        .set_text("#let x=1\n".to_owned());
    app.format_document();
    let edit = TextEdit {
        range: BytePos(0)..BytePos(app.session.tabs[index].editor.len_bytes()),
        new_text: "#let y=2\n".to_owned(),
    };
    assert!(app.session.tabs[index].editor.apply_byte_edits(&[edit]));
    let reply = app.format_worker.wait().expect("the format worker answers");
    app.apply_format_reply(reply);
    assert_eq!(
        app.session.tabs[index].editor.shared_text().as_str(),
        "#let y=2\n",
        "a reply for an older revision must not overwrite the buffer"
    );
    app.shutdown_background();
}

#[test]
fn harness_forced_persist_flushes_settings_and_clears_dirty() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    app.settings.editor.wrap = crate::settings::WrapPref::Markup;
    app.mark_settings_dirty();
    assert!(app.settings_dirty, "changing settings marks them dirty");

    let outcome = app.persist_settings(true);
    assert!(
        outcome.toast.is_none(),
        "the harness config dir is writable"
    );
    assert!(
        !app.settings_dirty,
        "a successful forced save clears the dirty flag"
    );

    let loaded = crate::settings::Settings::load();
    assert_eq!(
        loaded.settings.editor.wrap,
        crate::settings::WrapPref::Markup,
        "the forced save is durable"
    );

    app.shutdown_background();
}

#[test]
fn harness_read_only_settings_warn_once_and_stop_retrying() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    // Simulate a config file written by a newer schema version.
    app.settings.writable = false;

    app.mark_settings_dirty();
    let first = app.persist_settings(true);
    assert!(first.toast.is_some(), "the first change must warn");
    assert!(!app.settings_dirty, "the read-only change is not retried");

    app.mark_settings_dirty();
    let second = app.persist_settings(true);
    assert!(second.toast.is_none(), "the warning is one-shot");
    assert!(!app.settings_dirty);

    app.shutdown_background();
}

#[test]
fn harness_pops_a_pane_into_its_own_viewport() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = copy_test_project("popout");
    app.open_root(root.clone());
    pump_until(&ctx, &mut app, SETTLE, parse_settled, "initial parse");

    // A raw `egui::Context` embeds viewports, so the immediate viewport runs
    // the preview pane in an embedded window; the frame must not panic.
    let preview = crate::shell::layout::Pane::Preview as usize;
    app.layout.popped[preview] = true;
    drive_frames(&ctx, &mut app, 3);
    assert!(
        app.layout.popped[preview],
        "the popped flag survives the immediate-viewport frames"
    );

    app.layout.popped[preview] = false;
    drive_frames(&ctx, &mut app, 1);

    app.shutdown_background();
    cleanup(&root);
}

// ---- Collab sync harness (Phase 4) ----------------------------------------

/// Drives full frames until `pred` holds or `deadline` passes.
fn drive_until(
    ctx: &egui::Context,
    app: &mut TypEditApp,
    deadline: Duration,
    pred: impl Fn(&TypEditApp) -> bool,
) -> bool {
    let end = Instant::now() + deadline;
    loop {
        drive_frame(ctx, app);
        if pred(app) {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn collab_client_receives_server_changes_and_cursors() {
    use super::collab::SessionKind;
    use typ_edit_collab::{ChangeOp, ChangeSet, CollabServer, ServerConfig};

    let ctx = egui::Context::default();
    let server_root = scratch_dir("collab-server");
    std::fs::write(server_root.join("main.typ"), "x").expect("write main");
    let config = ServerConfig {
        root: server_root.clone(),
        project: "proj".to_owned(),
        main_file: Some("main.typ".to_owned()),
        server_user: "srv".to_owned(),
        port: 0,
        image_dir: server_root.clone(),
        max_connections: 16,
        idle_timeout: Some(Duration::from_secs(30)),
        ready_timeout: Duration::from_secs(30),
        initial_docs: Vec::new(),
    };
    let server = CollabServer::start(config, None).expect("server");
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], server.port()).into();

    let mut app = harness_app(&ctx, None);
    app.collab.begin_join(vec![addr], "ann".to_owned(), None);
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |app| {
            app.collab.kind() == SessionKind::Client
        }),
        "the client attaches and mirrors the project"
    );

    // (a) A server-local change reaches the client's open editor buffer.
    server.submit_local_change(ChangeSet::new(
        "main.typ",
        vec![ChangeOp::Insert {
            at: 0,
            text: "H".to_owned(),
        }],
    ));
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |app| {
            app.session.tabs[app.session.active_tab]
                .editor
                .shared_text()
                .as_str()
                == "Hx"
        }),
        "the open editor buffer updated from the server change"
    );
    assert!(
        app.session.tabs[app.session.active_tab].dirty,
        "the remotely-edited tab is marked dirty"
    );

    // (b) A local edit flushes to the server (and the echo is not double-applied).
    app.editor_mut().queue_insert_text("C".to_owned());
    drive_frames(&ctx, &mut app, 8);
    let received = drive_until(&ctx, &mut app, Duration::from_secs(10), |_| {
        server
            .query_doc("main.typ")
            .and_then(|(_, text)| text)
            .is_some_and(|text| text.contains('C'))
    });
    assert!(received, "the server received the client's local edit");
    assert!(
        app.session.tabs[app.session.active_tab]
            .editor
            .shared_text()
            .contains('C'),
        "the own echo did not double-apply (the buffer holds exactly one 'C')"
    );

    // (c) The server host's cursors appear in the client's table.
    server.submit_local_cursors("main.typ", vec![1], vec![(0, 1)]);
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |app| {
            app.collab.cursors.contains_key(&0)
        }),
        "the client's cursor table received the server host's cursor"
    );
    assert!(
        app.collab
            .remote_carets_for("main.typ")
            .iter()
            .any(|caret| caret.carets == vec![1]),
        "the cursor table maps to a remote caret for the active file"
    );

    server.stop();
    cleanup(&server_root);
    app.shutdown_background();
}

// ---- Collab review fixes: resync, images, join cancel ----------------------

/// Connects a raw client, waits for its snapshot and sends `Ready`.
fn tiny_png() -> Vec<u8> {
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer.write_image_data(&[0, 0, 0, 255]).expect("png data");
    }
    png
}

fn join_raw_client(addr: std::net::SocketAddr) -> typ_edit_collab::CollabClient {
    use typ_edit_collab::{ClientEvent, CollabClient};
    let client = CollabClient::connect(&[addr], "bob", None).expect("raw connect");
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        while let Some(event) = client.try_recv_event() {
            if matches!(event, ClientEvent::SnapshotComplete) {
                client.ready();
                return client;
            }
        }
        assert!(Instant::now() < end, "raw client snapshot did not complete");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Requests `path` from the server and waits for the matching `FileBin`.
fn raw_get_file(
    client: &typ_edit_collab::CollabClient,
    path: &str,
    deadline: Duration,
) -> Option<Vec<u8>> {
    use typ_edit_collab::ClientEvent;
    client.get_file(path);
    let end = Instant::now() + deadline;
    loop {
        while let Some(event) = client.try_recv_event() {
            if let ClientEvent::FileBin { path: got, bytes } = event
                && got == path
            {
                return Some(bytes);
            }
        }
        if Instant::now() >= end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn collab_forced_desync_resyncs_and_converges() {
    use super::collab::SessionKind;
    use typ_edit_collab::{ChangeOp, ChangeSet, CollabServer, ServerConfig};

    let ctx = egui::Context::default();
    let server_root = scratch_dir("collab-desync");
    std::fs::write(server_root.join("main.typ"), "base").expect("write main");
    let config = ServerConfig {
        root: server_root.clone(),
        project: "proj".to_owned(),
        main_file: Some("main.typ".to_owned()),
        server_user: "srv".to_owned(),
        port: 0,
        image_dir: server_root.clone(),
        max_connections: 16,
        idle_timeout: Some(Duration::from_secs(30)),
        ready_timeout: Duration::from_secs(30),
        initial_docs: Vec::new(),
    };
    let server = CollabServer::start(config, None).expect("server");
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], server.port()).into();

    let mut app = harness_app(&ctx, None);
    app.collab.begin_join(vec![addr], "ann".to_owned(), None);
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |app| {
            app.collab.kind() == SessionKind::Client
                && app.session.tabs[app.session.active_tab]
                    .editor
                    .shared_text()
                    .as_str()
                    == "base"
        }),
        "the client mirrors and opens the project"
    );
    let mirror = app.tree.root.clone().expect("mirror root");

    // Force a Desync: an invalid change set is rejected atomically by the server.
    app.collab.send_changes(ChangeSet::new(
        "main.typ",
        vec![ChangeOp::Delete {
            at: 0,
            text: "ZZZ".to_owned(),
        }],
    ));
    // A second client commits a valid change the resync must end up including.
    let b = join_raw_client(addr);
    b.send_changes(ChangeSet::new(
        "main.typ",
        vec![ChangeOp::Insert {
            at: 0,
            text: "B".to_owned(),
        }],
    ));

    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |app| {
            app.session.tabs[app.session.active_tab]
                .editor
                .shared_text()
                .as_str()
                == "Bbase"
        }),
        "the resync applied the snapshot to the open buffer"
    );
    // Mirror writes are coalesced for 100ms, so the file catches up shortly
    // after the buffer; wait for that bounded flush instead of assuming it is
    // synchronous with the frame that applied the change.
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |_| {
            std::fs::read_to_string(mirror.join("main.typ")).is_ok_and(|text| text == "Bbase")
        }),
        "the mirror equals the authoritative content after the debounced flush"
    );
    assert_eq!(
        server
            .query_doc("main.typ")
            .and_then(|(_, text)| text)
            .as_deref(),
        Some("Bbase")
    );

    // Later edits continue to sync after the resync.
    app.editor_mut().queue_insert_text("Z".to_owned());
    drive_frames(&ctx, &mut app, 8);
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |_| {
            server
                .query_doc("main.typ")
                .and_then(|(_, text)| text)
                .is_some_and(|text| text.contains('Z'))
        }),
        "edits continue to sync after the resync"
    );

    drop(b);
    server.stop();
    cleanup(&server_root);
    app.shutdown_background();
}

#[test]
fn collab_image_round_trip_through_image_dir() {
    use super::collab::SessionKind;
    use typ_edit_collab::{CollabServer, ServerConfig};

    let ctx = egui::Context::default();
    let server_root = scratch_dir("collab-image");
    std::fs::write(server_root.join("main.typ"), "body\n").expect("write main");
    let image_dir = server_root.join("image");
    let config = ServerConfig {
        root: server_root.clone(),
        project: "proj".to_owned(),
        main_file: Some("main.typ".to_owned()),
        server_user: "srv".to_owned(),
        port: 0,
        image_dir: image_dir.clone(),
        max_connections: 16,
        idle_timeout: Some(Duration::from_secs(30)),
        ready_timeout: Duration::from_secs(30),
        initial_docs: Vec::new(),
    };
    let server = CollabServer::start(config, None).expect("server");
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], server.port()).into();

    let mut app = harness_app(&ctx, None);
    app.collab.begin_join(vec![addr], "ann".to_owned(), None);
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |app| {
            app.collab.kind() == SessionKind::Client
                && app.session.tabs[app.session.active_tab]
                    .editor
                    .shared_text()
                    .as_str()
                    == "body\n"
        }),
        "the client mirrors and opens the project"
    );
    let mirror = app.tree.root.clone().expect("mirror root");
    // Put the caret at the empty line after the body so the snippet insertion is
    // not refused next to the identifier.
    app.editor_mut().jump_to_byte(5);
    drive_frames(&ctx, &mut app, 2);

    let bytes = vec![0x89u8, b'P', b'N', b'G', 1, 2, 3, 4];
    assert!(
        app.collab
            .upload_image("pasted.png", bytes.clone())
            .is_some(),
        "a client session accepts an upload"
    );

    // The server stores it in the in-root image dir; the uploader fetches it into
    // its mirror and inserts a snippet whose path resolves relative to the doc.
    let inserted = drive_until(&ctx, &mut app, Duration::from_secs(10), |app| {
        app.session.tabs[app.session.active_tab]
            .editor
            .shared_text()
            .contains("#image(\"image/pasted.png\")")
    });
    assert!(
        inserted,
        "the uploader inserted a snippet with the stored image path; buffer={:?}",
        app.session.tabs[app.session.active_tab]
            .editor
            .shared_text()
    );
    assert_eq!(
        std::fs::read(image_dir.join("pasted.png")).expect("server image"),
        bytes,
        "the server stored the image under its in-root image dir"
    );
    let mirror_image = mirror.join("image").join("pasted.png");
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |_| mirror_image
            .exists()),
        "the uploader fetched its own image into the mirror"
    );
    assert_eq!(
        std::fs::read(&mirror_image).expect("mirror image"),
        bytes,
        "the uploader fetched its own image into the mirror"
    );

    // A second client can fetch it too.
    let b = join_raw_client(addr);
    let fetched = raw_get_file(&b, "image/pasted.png", Duration::from_secs(10));
    assert_eq!(fetched.as_deref(), Some(bytes.as_slice()));

    drop(b);
    server.stop();
    cleanup(&server_root);
    app.shutdown_background();
}

#[test]
fn collab_join_cancel_does_not_open_the_mirror() {
    use super::collab::SessionKind;
    use typ_edit_collab::{CollabServer, ServerConfig};

    let ctx = egui::Context::default();
    let server_root = scratch_dir("collab-cancel");
    std::fs::write(server_root.join("main.typ"), "base").expect("write main");
    let config = ServerConfig {
        root: server_root.clone(),
        project: "proj".to_owned(),
        main_file: Some("main.typ".to_owned()),
        server_user: "srv".to_owned(),
        port: 0,
        image_dir: server_root.clone(),
        max_connections: 16,
        idle_timeout: Some(Duration::from_secs(30)),
        ready_timeout: Duration::from_secs(30),
        initial_docs: Vec::new(),
    };
    let server = CollabServer::start(config, None).expect("server");
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], server.port()).into();

    let mut app = harness_app(&ctx, None);
    app.collab.begin_join(vec![addr], "ann".to_owned(), None);
    app.collab.close_join();

    // Drive well past the connect window: no session may install.
    let end = Instant::now() + Duration::from_millis(400);
    while Instant::now() < end {
        drive_frame(&ctx, &mut app);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        app.collab.kind(),
        SessionKind::Off,
        "cancelling during connect installs no session"
    );
    assert!(app.tree.root.is_none(), "cancelling opens no mirror");

    server.stop();
    cleanup(&server_root);
    app.shutdown_background();
}

#[test]
fn collab_joined_seeds_the_cursor_table() {
    use typ_edit_collab::ClientEvent;
    // A `Joined` carrying cursors seeds the controller's table (M3), keyed by sid
    // with the user name resolved from the `users` list.
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    // Drive an injected Joined through the controller's public seam.
    let users = vec![typ_edit_collab::UserInfo {
        sid: 7,
        user: "zoe".to_owned(),
    }];
    let cursors = vec![typ_edit_collab::CursorState {
        sid: 7,
        file: "a.typ".to_owned(),
        carets: vec![3],
        selections: vec![(1, 3)],
    }];
    app.collab.inject_event_for_test(ClientEvent::Joined {
        sid: 7,
        generation: 0,
        project: "proj".to_owned(),
        server_user: "srv".to_owned(),
        users,
        cursors,
    });
    let carets = app.collab.remote_carets_for("a.typ");
    assert_eq!(carets.len(), 1);
    assert_eq!(carets[0].user, "zoe");
    assert_eq!(carets[0].carets, vec![3]);
    app.shutdown_background();
}

#[test]
fn collab_join_closes_the_panel_and_keeps_editing_live() {
    use super::collab::{JoinFlow, SessionKind};
    use typ_edit_collab::{CollabServer, ServerConfig};

    let ctx = egui::Context::default();
    let server_root = scratch_dir("collab-panel");
    std::fs::write(server_root.join("main.typ"), "base").expect("write main");
    let config = ServerConfig {
        root: server_root.clone(),
        project: "proj".to_owned(),
        main_file: Some("main.typ".to_owned()),
        server_user: "srv".to_owned(),
        port: 0,
        image_dir: server_root.clone(),
        max_connections: 16,
        idle_timeout: Some(Duration::from_secs(30)),
        ready_timeout: Duration::from_secs(30),
        initial_docs: Vec::new(),
    };
    let server = CollabServer::start(config, None).expect("server");
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], server.port()).into();

    let mut app = harness_app(&ctx, None);
    app.collab.open_join_dialog(&app.settings);
    assert!(app.collab.join_open, "the join panel starts open");
    app.collab.join_username = "ann".to_owned();
    app.collab.begin_join(vec![addr], "ann".to_owned(), None);
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |app| {
            app.collab.kind() == SessionKind::Client && app.collab.join == JoinFlow::Live
        }),
        "the client reaches Live"
    );
    assert!(
        !app.collab.join_open,
        "a successful join must close the panel, not leave Cancel as a trap"
    );

    // Closing the panel after Live is a no-op for the session; local edits must
    // still reach the server (the old code set `join = Idle` here and stopped
    // flushing captures).
    app.collab.close_join();
    assert_eq!(app.collab.kind(), SessionKind::Client);
    app.editor_mut().queue_insert_text("Z".to_owned());
    drive_frames(&ctx, &mut app, 8);
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |_| {
            server
                .query_doc("main.typ")
                .and_then(|(_, text)| text)
                .is_some_and(|text| text.contains('Z'))
        }),
        "closing the panel must not stop local edit sync"
    );

    server.stop();
    cleanup(&server_root);
    app.shutdown_background();
}

#[test]
fn collab_manual_join_uses_the_password_field() {
    use super::collab::{JoinFlow, SessionKind};
    use typ_edit_collab::{CollabServer, ServerConfig};

    let ctx = egui::Context::default();
    let server_root = scratch_dir("collab-manual-pw");
    std::fs::write(server_root.join("main.typ"), "secret").expect("write main");
    let config = ServerConfig {
        root: server_root.clone(),
        project: "proj".to_owned(),
        main_file: Some("main.typ".to_owned()),
        server_user: "srv".to_owned(),
        port: 0,
        image_dir: server_root.clone(),
        max_connections: 16,
        idle_timeout: Some(Duration::from_secs(30)),
        ready_timeout: Duration::from_secs(30),
        initial_docs: Vec::new(),
    };
    let server = CollabServer::start(config, Some("hunter2".to_owned())).expect("server");
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], server.port()).into();

    let mut app = harness_app(&ctx, None);
    app.collab.join_username = "ann".to_owned();
    app.collab.join_address = addr.to_string();
    app.collab.join_password = "hunter2".to_owned();
    assert!(app.collab.join_manual().is_none());
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |app| {
            app.collab.kind() == SessionKind::Client && app.collab.join == JoinFlow::Live
        }),
        "a manual ip:port join to a password room uses the field"
    );

    server.stop();
    cleanup(&server_root);
    app.shutdown_background();
}

#[test]
fn collab_ui_clicks_open_join_and_connects() {
    use super::collab::{JoinFlow, SessionKind};
    use super::ui_probe;
    use typ_edit_collab::{CollabServer, ServerConfig};

    let ctx = egui::Context::default();
    let server_root = scratch_dir("collab-ui-click");
    std::fs::write(server_root.join("main.typ"), "base").expect("write main");
    let config = ServerConfig {
        root: server_root.clone(),
        project: "proj".to_owned(),
        main_file: Some("main.typ".to_owned()),
        server_user: "srv".to_owned(),
        port: 0,
        image_dir: server_root.clone(),
        max_connections: 16,
        idle_timeout: Some(Duration::from_secs(30)),
        ready_timeout: Duration::from_secs(30),
        initial_docs: Vec::new(),
    };
    let server = CollabServer::start_with_startup_delay(config, None, Duration::from_secs(6))
        .expect("server");
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], server.port()).into();

    let mut app = harness_app(&ctx, None);
    ui_probe::clear();
    drive_frames(&ctx, &mut app, 3);

    let open = ui_probe::rect("landing.open").expect("landing open button");
    click_at(&ctx, &mut app, open.center());
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(2), |_| {
            ui_probe::rect("landing.join").is_some()
        }),
        "clicking Open must show the join menu item"
    );
    let join = ui_probe::rect("landing.join").expect("join menu item");
    click_at(&ctx, &mut app, join.center());
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(2), |app| {
            app.collab.join_open
        }),
        "clicking the join menu item must open the join dialog"
    );

    let username = ui_probe::rect("collab.join.username").expect("username input");
    click_at(&ctx, &mut app, username.center());
    type_text(&ctx, &mut app, "ann");
    let address = ui_probe::rect("collab.join.address").expect("address input");
    click_at(&ctx, &mut app, address.center());
    type_text(&ctx, &mut app, &addr.to_string());
    let connect = ui_probe::rect("collab.join.connect").expect("connect button");
    click_at(&ctx, &mut app, connect.center());

    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(15), |app| {
            app.collab.kind() == SessionKind::Client && app.collab.join == JoinFlow::Live
        }),
        "the real button path must reach Live; state={:?}, toast={:?}",
        app.collab.join,
        app.ui.toast.as_ref().map(|toast| &toast.message)
    );
    assert!(!app.collab.join_open, "a successful join closes the panel");
    assert!(app.tree.root.is_some(), "the mirror workspace opened");

    server.stop();
    cleanup(&server_root);
    app.shutdown_background();
}

#[test]
fn collab_modal_and_menu_block_background_clicks() {
    use super::ui_probe;

    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    ui_probe::clear();
    drive_frames(&ctx, &mut app, 3);

    let open = ui_probe::rect("landing.open").expect("landing open button");
    let settings = ui_probe::rect("landing.settings").expect("landing settings button");
    let base_layer = ctx.layer_id_at(settings.center());
    click_at(&ctx, &mut app, open.center());
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(2), |_| {
            ui_probe::rect("landing.join").is_some()
        }),
        "the open menu must appear"
    );
    assert!(
        ctx.memory(|memory| memory.top_modal_layer().is_some()),
        "the menu backdrop must register a modal barrier for this click"
    );
    let settings = ui_probe::rect("landing.settings").expect("landing settings button");
    assert_ne!(
        ctx.layer_id_at(settings.center()),
        base_layer,
        "an open menu must own the hover layer above the page"
    );
    click_at(&ctx, &mut app, settings.center());
    drive_frames(&ctx, &mut app, 2);
    assert!(
        !app.ui.settings_open,
        "a click below an open menu must not reach the settings button"
    );
    egui::Popup::close_all(&ctx);
    drive_frames(&ctx, &mut app, 2);
    ui_probe::clear();
    drive_frames(&ctx, &mut app, 2);

    let open = ui_probe::rect("landing.open").expect("landing open button");
    click_at(&ctx, &mut app, open.center());
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(2), |_| {
            ui_probe::rect("landing.join").is_some()
        }),
        "the open menu must reopen"
    );
    let join = ui_probe::rect("landing.join").expect("join menu item");
    click_at(&ctx, &mut app, join.center());
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(2), |app| {
            app.collab.join_open
        }),
        "the join dialog must open"
    );
    assert!(
        !app.ui.typst_help_open && !app.ui.shortcuts_open,
        "clicking a menu row must not click a landing link behind the popup"
    );
    let settings = ui_probe::rect("landing.settings").expect("landing settings button");
    assert_ne!(
        ctx.layer_id_at(settings.center()),
        base_layer,
        "an open modal must own the hover layer above the page"
    );
    click_at(&ctx, &mut app, settings.center());
    drive_frames(&ctx, &mut app, 2);
    assert!(
        !app.ui.settings_open,
        "a click below a modal dialog must not reach the settings button"
    );

    app.shutdown_background();
}

#[test]
fn top_menu_item_click_does_not_move_the_editor_caret() {
    use super::ui_probe;

    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = copy_test_project("menu-caret");
    app.open_root(root.clone());
    drive_frames(&ctx, &mut app, 4);

    let before = app.compile.cursor_byte;
    let edit = ui_probe::rect("menu.edit.button").expect("edit menu button");
    click_at(&ctx, &mut app, edit.center());
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(2), |_| {
            ui_probe::rect("menu.edit.find").is_some()
        }),
        "the edit menu must open"
    );
    let find = ui_probe::rect("menu.edit.find").expect("find menu row");
    click_at(&ctx, &mut app, find.center());
    drive_frames(&ctx, &mut app, 2);
    assert_eq!(
        app.compile.cursor_byte, before,
        "a menu-row click must not pass through to the editor"
    );

    cleanup(&root);
    app.shutdown_background();
}

#[test]
fn collab_fetches_a_late_missing_asset_and_recompiles() {
    use super::collab::{JoinFlow, SessionKind};
    use typ_edit_collab::{CollabServer, ServerConfig};

    let ctx = egui::Context::default();
    let server_root = scratch_dir("collab-late-asset");
    std::fs::write(server_root.join("main.typ"), "base").expect("write main");
    let config = ServerConfig {
        root: server_root.clone(),
        project: "proj".to_owned(),
        main_file: Some("main.typ".to_owned()),
        server_user: "srv".to_owned(),
        port: 0,
        image_dir: server_root.join("image"),
        max_connections: 16,
        idle_timeout: Some(Duration::from_secs(30)),
        ready_timeout: Duration::from_secs(30),
        initial_docs: Vec::new(),
    };
    let server = CollabServer::start(config, None).expect("server");
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], server.port()).into();
    let mut app = harness_app(&ctx, None);
    app.collab.begin_join(vec![addr], "ann".to_owned(), None);
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |app| {
            app.collab.kind() == SessionKind::Client && app.collab.join == JoinFlow::Live
        }),
        "the client reaches Live"
    );
    let mirror = app.tree.root.clone().expect("mirror root");

    std::fs::create_dir_all(server_root.join("image")).expect("server image dir");
    let png = tiny_png();
    std::fs::write(server_root.join("image/late.png"), &png).expect("write late image");

    app.editor_mut()
        .set_text("#image(\"image/late.png\")\n".to_owned());
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(15), |app| {
            mirror.join("image/late.png").exists()
                && app.compile.doc.is_some()
                && app.downloads.active_count() == 0
        }),
        "the missing image must be fetched and the document recompiled; downloads={:?}, toast={:?}",
        app.downloads.active_count(),
        app.ui.toast.as_ref().map(|toast| &toast.message)
    );

    server.stop();
    cleanup(&server_root);
    app.shutdown_background();
}

#[test]
fn collab_background_tasks_button_opens_only_with_active_tasks() {
    use super::ui_probe;

    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = scratch_dir("background-tasks");
    std::fs::write(root.join("main.typ"), "base").expect("write main");
    app.open_root(root.clone());
    drive_frames(&ctx, &mut app, 4);
    ui_probe::clear();
    drive_frames(&ctx, &mut app, 2);

    assert!(
        ui_probe::rect("status.background_tasks").is_none(),
        "zero active tasks must hide the label and its separator"
    );

    assert!(app.downloads.begin("image/pending.png"));
    drive_frames(&ctx, &mut app, 2);
    let label = ui_probe::rect("status.background_tasks").expect("tasks label");
    click_at(&ctx, &mut app, label.center());
    drive_frames(&ctx, &mut app, 2);
    assert!(
        app.downloads.panel_open,
        "a positive task count must open the background panel"
    );

    cleanup(&root);
    app.shutdown_background();
}

#[test]
fn collab_join_hydrates_existing_images() {
    use super::collab::{JoinFlow, SessionKind};
    use typ_edit_collab::{CollabServer, ServerConfig};

    let ctx = egui::Context::default();
    let server_root = scratch_dir("collab-join-image");
    std::fs::create_dir_all(server_root.join("image")).expect("server image dir");
    std::fs::write(server_root.join("main.typ"), "#image(\"image/pic.png\")\n")
        .expect("write main");
    std::fs::write(server_root.join("image/pic.png"), tiny_png()).expect("write image");
    let config = ServerConfig {
        root: server_root.clone(),
        project: "proj".to_owned(),
        main_file: Some("main.typ".to_owned()),
        server_user: "srv".to_owned(),
        port: 0,
        image_dir: server_root.join("image"),
        max_connections: 16,
        idle_timeout: Some(Duration::from_secs(30)),
        ready_timeout: Duration::from_secs(30),
        initial_docs: Vec::new(),
    };
    let server = CollabServer::start(config, None).expect("server");
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], server.port()).into();
    let mut app = harness_app(&ctx, None);
    app.collab.begin_join(vec![addr], "ann".to_owned(), None);
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(15), |app| {
            app.collab.kind() == SessionKind::Client
                && app.collab.join == JoinFlow::Live
                && app
                    .tree
                    .root
                    .as_ref()
                    .is_some_and(|root| root.join("image/pic.png").exists())
                && app.compile.doc.is_some()
                && app.downloads.active_count() == 0
        }),
        "the join snapshot must hydrate existing images before opening the mirror"
    );

    server.stop();
    cleanup(&server_root);
    app.shutdown_background();
}

#[test]
fn collab_host_undo_updates_the_host_buffer() {
    use super::collab::ServerStart;
    use typ_edit_collab::HistoryOp;

    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = copy_test_project("collab-host-undo");
    let base = std::fs::read_to_string(root.join("main.typ")).expect("read main");
    app.open_root(root.clone());
    drive_frames(&ctx, &mut app, 2);

    let start = ServerStart {
        root: root.clone(),
        main_file: Some(root.join("main.typ")),
        username: "host".to_owned(),
        password: None,
        port: 0,
        initial_docs: Vec::new(),
    };
    app.collab
        .start_server(&mut app.settings, start)
        .expect("server starts");

    app.editor_mut().queue_insert_text("H".to_owned());
    drive_frames(&ctx, &mut app, 8);
    // The server records the host's normal edit and broadcasts HistoryState;
    // this proves the change is at the frontier before the undo.
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |app| {
            app.collab.history_state().0
        }),
        "the host edit is recorded on the server history"
    );
    assert!(
        app.session.tabs[app.session.active_tab]
            .editor
            .shared_text()
            .starts_with('H')
    );

    app.collab.request_history(HistoryOp::Undo);
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |app| {
            app.session.tabs[app.session.active_tab]
                .editor
                .shared_text()
                .as_str()
                == base
        }),
        "a host undo routed to the server must be applied to the host buffer"
    );
    assert!(
        drive_until(&ctx, &mut app, Duration::from_secs(10), |app| {
            app.collab.history_state() == (false, true)
        }),
        "the post-undo HistoryState must report no undo and an available redo"
    );

    app.collab.stop_server();
    cleanup(&root);
    app.shutdown_background();
}

#[test]
fn collab_share_does_not_infer_an_open_file_as_main() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = scratch_dir("collab-no-main");
    std::fs::write(root.join("one.typ"), "= One").expect("write one");
    std::fs::write(root.join("two.typ"), "= Two").expect("write two");
    app.open_root(root.clone());
    drive_frames(&ctx, &mut app, 2);

    assert!(
        app.tree.main_file.is_none(),
        "a project without a marker has no explicit main"
    );
    let active = app.editor().path.clone().expect("a source file is open");
    assert!(
        app.share_main_file().is_none(),
        "the host's active file must not become the advertised main"
    );
    app.tree.main_file = Some(active.clone());
    assert_eq!(app.share_main_file(), Some(active));

    app.shutdown_background();
    cleanup(&root);
}

#[test]
fn collab_share_seeds_dirty_open_buffers() {
    use super::collab::ServerStart;
    use typ_edit_collab::{ClientEvent, CollabClient};

    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let root = copy_test_project("collab-seed");
    app.open_root(root.clone());
    drive_frames(&ctx, &mut app, 2);
    app.editor_mut().queue_insert_text("H".to_owned());
    drive_frames(&ctx, &mut app, 4);
    assert!(app.session.tabs[app.session.active_tab].dirty);

    let docs = app.collab_initial_docs();
    let (_, text) = docs
        .iter()
        .find(|(path, _)| path == "main.typ")
        .expect("the dirty main buffer is collected");
    assert!(
        text.materialize().starts_with('H'),
        "the collected text is the buffer"
    );

    let start = ServerStart {
        root: root.clone(),
        main_file: Some(root.join("main.typ")),
        username: "host".to_owned(),
        password: None,
        port: 0,
        initial_docs: docs,
    };
    let toast = app
        .collab
        .start_server(&mut app.settings, start)
        .expect("server starts");
    assert_eq!(toast.kind, crate::theme::SemanticKind::Success);
    let port = app.collab.server_port().expect("bound port");
    let client = CollabClient::connect(&[([127, 0, 0, 1], port).into()], "ann", None)
        .expect("client connects");

    let mut main_text = None;
    let mut done = false;
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end && !done {
        while let Some(event) = client.try_recv_event() {
            match event {
                ClientEvent::FileText { path, text } if path == "main.typ" => {
                    main_text = Some(text)
                }
                ClientEvent::SnapshotComplete => done = true,
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(done, "the join snapshot completes");
    assert!(
        main_text.expect("main.typ text").starts_with('H'),
        "the joiner sees the host's unsaved buffer"
    );

    drop(client);
    app.collab.stop_server();
    cleanup(&root);
    app.shutdown_background();
}

#[test]
fn ui_scale_scales_the_whole_interface() {
    let ctx = egui::Context::default();
    let mut settings = Settings::default();
    settings.appearance.ui_scale = 2.0;
    let mut app = harness_app_with(&ctx, settings, None);
    drive_frame(&ctx, &mut app);
    assert!(
        (ctx.zoom_factor() - 2.0).abs() < 1e-6,
        "200% UI scale must apply a 2x zoom, got {}",
        ctx.zoom_factor()
    );
    app.shutdown_background();
}

#[test]
fn ui_font_size_changes_text_styles_not_zoom() {
    let ctx = egui::Context::default();
    let mut settings = Settings::default();
    settings.fonts.ui_font_size = 20.0;
    let mut app = harness_app_with(&ctx, settings, None);
    drive_frame(&ctx, &mut app);
    assert_eq!(ctx.zoom_factor(), 1.0);
    let body = ctx
        .style_of(egui::Theme::Dark)
        .text_styles
        .get(&egui::TextStyle::Body)
        .expect("body style")
        .size;
    assert_eq!(body, 20.0);
    app.shutdown_background();
}

#[test]
fn landing_help_links_open_shortcuts_and_typst_help() {
    use super::ui_probe;

    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    ui_probe::clear();
    drive_frames(&ctx, &mut app, 3);

    let help = ui_probe::rect("landing.typst_help").expect("Typst help link");
    click_at(&ctx, &mut app, help.center());
    drive_frames(&ctx, &mut app, 2);
    assert!(
        app.ui.typst_help_open,
        "Typst help link must open the modal"
    );

    app.ui.typst_help_open = false;
    drive_frames(&ctx, &mut app, 2);
    let shortcuts = ui_probe::rect("landing.shortcuts").expect("shortcuts link");
    click_at(&ctx, &mut app, shortcuts.center());
    drive_frames(&ctx, &mut app, 2);
    assert!(app.ui.shortcuts_open, "shortcuts link must open the window");

    app.shutdown_background();
}

#[test]
fn project_search_renders_results_when_a_project_is_open() {
    use super::search::LeftView;
    use super::ui_probe;

    let ctx = egui::Context::default();
    let root = scratch_dir("search-results");
    let mut app = harness_app(&ctx, Some(root.clone()));
    drive_frames(&ctx, &mut app, 2);
    assert_eq!(app.tree.root.as_deref(), Some(root.as_path()));

    app.left_view = LeftView::Search;
    app.search.query = "needle".to_owned();
    app.search.result = Some(ProjectSearchResult {
        generation: 1,
        files: vec![FileMatches {
            path: root.join("foo.typ"),
            relative: "foo.typ".to_owned(),
            matches: vec![SearchMatch {
                line: 0,
                byte: BytePos(0),
                byte_len: 6,
                preview: "needle".to_owned(),
                preview_match: CharPos(0)..CharPos(6),
            }],
        }],
        total_matches: 1,
        truncated: false,
        error: None,
    });
    ui_probe::clear();
    drive_frame(&ctx, &mut app);

    assert!(
        ui_probe::rect("search.results").is_some(),
        "an open workspace must render the stored search results"
    );
    assert!(
        ui_probe::rect("search.no_project").is_none(),
        "the no-project hint must not replace results when a root is open"
    );

    app.shutdown_background();
    cleanup(&root);
}

#[test]
fn project_lock_follows_project_switches_and_releases() {
    let ctx = egui::Context::default();
    let mut app = harness_app(&ctx, None);
    let first = scratch_dir("project-lock-first");
    let second = scratch_dir("project-lock-second");
    std::fs::write(first.join("main.typ"), "= First").expect("write first main");
    std::fs::write(second.join("main.typ"), "= Second").expect("write second main");

    app.open_root(first.clone());
    assert!(crate::workspace::lock_path(&first).is_file());

    app.open_root(second.clone());
    assert!(!crate::workspace::lock_path(&first).exists());
    assert!(crate::workspace::lock_path(&second).is_file());

    app.release_project_lock();
    assert!(!crate::workspace::lock_path(&second).exists());

    app.shutdown_background();
    cleanup(&first);
    cleanup(&second);
}
