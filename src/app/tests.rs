use super::editor_pane::hover_due;
use super::errors::first_error_message;
use super::files::{
    classify_startup_path, dirty_tabs_under, format_on_save_applies, is_typst_path,
    is_typst_source, relative_snippet_path, reroot_path,
};
use super::frame::{SETTINGS_PERSIST_DEBOUNCE, settings_persist_due};
use super::gate::{EditorGate, LARGE_OPEN_GATE_BYTES};
use super::ide::{apply_edits_to_text, completions_moved_away, edit_counts};
use super::image_import::insert::{ImageForm, InsertDecision, RejectReason, decide_insertion};
use super::image_import::{
    combined_snippets, is_supported_image, references_in_place, unique_image_path,
};
use super::preview::follow_due;
use super::state::ExportState;
use super::tabs::{active_slot_reusable, clamp_caret, find_open_tab, tab_display_name};
use super::window_chrome::{TITLE_BAR_H, resize_direction};
use super::{EditorTab, ErrorRow, StartupTarget};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use typ_edit_editor::EditorState;
use typ_edit_engine::accept::{accepts_exact_current, accepts_tab_latest};
use typ_edit_model::{BytePos, FileEdits, ParseResult, TextEdit};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Unique, self-cleaning scratch directory under the OS temp dir.
fn scratch_dir(tag: &str) -> PathBuf {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "typ-edit-app-test-{}-{seq}-{nanos}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[test]
fn classify_directory_as_workspace() {
    let dir = scratch_dir("dir");
    assert_eq!(
        classify_startup_path(&dir),
        StartupTarget::Workspace(dir.clone())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn classify_typ_file_as_file() {
    let dir = scratch_dir("typ");
    let file = dir.join("doc.typ");
    std::fs::write(&file, "= Title").expect("write typ file");
    assert_eq!(
        classify_startup_path(&file),
        StartupTarget::File(file.clone())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn classify_non_typ_file_as_ignore() {
    let dir = scratch_dir("txt");
    let file = dir.join("notes.txt");
    std::fs::write(&file, "hello").expect("write text file");
    assert_eq!(classify_startup_path(&file), StartupTarget::Ignore);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn classify_missing_path_as_ignore() {
    let dir = scratch_dir("missing");
    let missing = dir.join("nope.typ");
    assert_eq!(classify_startup_path(&missing), StartupTarget::Ignore);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn is_typst_path_matches_the_typ_extension_case_insensitively() {
    assert!(is_typst_path(Path::new("doc.typ")));
    assert!(is_typst_path(Path::new("DOC.TYP")));
    assert!(!is_typst_path(Path::new("notes.txt")));
    assert!(!is_typst_path(Path::new("doc.typ.bak")));
    assert!(!is_typst_path(Path::new("README")));
}

#[test]
fn is_typst_source_defaults_an_unnamed_scratch_to_typst() {
    assert!(is_typst_source(None), "an unnamed scratch buffer is Typst");
    assert!(is_typst_source(Some(Path::new("a.typ"))));
    assert!(!is_typst_source(Some(Path::new("a.txt"))));
}

#[test]
fn format_on_save_applies_only_to_typst_sources_when_enabled() {
    assert!(format_on_save_applies(true, Path::new("doc.typ")));
    assert!(format_on_save_applies(true, Path::new("DOC.TYP")));
    assert!(!format_on_save_applies(false, Path::new("doc.typ")));
    assert!(!format_on_save_applies(true, Path::new("notes.txt")));
}

#[test]
fn find_open_tab_matches_paths_exactly() {
    let a = PathBuf::from("/w/a.typ");
    let b = PathBuf::from("/w/b.typ");
    let tabs = [None, Some(a.clone()), Some(b.clone())];
    assert_eq!(find_open_tab(&tabs, &a), Some(1));
    assert_eq!(find_open_tab(&tabs, &b), Some(2));
    assert_eq!(find_open_tab(&tabs, Path::new("/w/c.typ")), None);
    assert_eq!(
        find_open_tab(&tabs, Path::new("a.typ")),
        None,
        "a relative path is not an exact match"
    );
    assert_eq!(find_open_tab(&[None], &a), None);
}

#[test]
fn tab_display_name_keeps_unique_names_plain() {
    let tabs = [
        Some(PathBuf::from("/w/docs/a.typ")),
        Some(PathBuf::from("/w/notes/b.typ")),
    ];
    assert_eq!(tab_display_name(0, &tabs), "a.typ");
    assert_eq!(tab_display_name(1, &tabs), "b.typ");
}

#[test]
fn tab_display_name_prefixes_parent_on_collision() {
    let tabs = [
        Some(PathBuf::from("/w/docs/main.typ")),
        Some(PathBuf::from("/w/notes/main.typ")),
    ];
    assert_eq!(tab_display_name(0, &tabs), "docs/main.typ");
    assert_eq!(tab_display_name(1, &tabs), "notes/main.typ");
}

#[test]
fn tab_display_name_names_untitled_and_out_of_range() {
    assert_eq!(tab_display_name(0, &[None]), "未命名");
    assert_eq!(tab_display_name(7, &[None]), "未命名");
}

#[test]
fn tab_display_name_keeps_bare_name_without_a_parent() {
    let tabs = [
        Some(PathBuf::from("main.typ")),
        Some(PathBuf::from("main.typ")),
    ];
    assert_eq!(tab_display_name(0, &tabs), "main.typ");
    assert_eq!(tab_display_name(1, &tabs), "main.typ");
}

#[test]
fn active_slot_reusable_only_for_clean_empty_untitled() {
    let tabs = [None, Some(PathBuf::from("/w/a.typ"))];
    assert!(active_slot_reusable(&tabs, 0, true, false));
    assert!(
        !active_slot_reusable(&tabs, 0, false, false),
        "non-empty text must not be overwritten"
    );
    assert!(
        !active_slot_reusable(&tabs, 0, true, true),
        "a dirty scratch buffer must not be reused"
    );
    assert!(
        !active_slot_reusable(&tabs, 1, true, false),
        "a named tab is not reusable"
    );
    assert!(
        !active_slot_reusable(&tabs, 2, true, false),
        "an out-of-range index is not reusable"
    );
}

fn tab(path: Option<&str>, dirty: bool) -> EditorTab {
    let mut editor = EditorState::new();
    editor.path = path.map(PathBuf::from);
    EditorTab {
        id: 0,
        editor,
        dirty,
    }
}

#[test]
fn new_editor_tab_starts_empty_and_clean() {
    let tab = EditorTab::new(0);
    assert!(
        tab.editor.shared_text().is_empty(),
        "a new tab starts with an empty buffer"
    );
    assert!(tab.editor.path.is_none(), "a new tab has no file path");
    assert!(!tab.dirty, "a new tab is not dirty");
}

#[test]
fn stale_parse_from_another_tab_is_never_applied() {
    let active_tab = 2;

    // A result that arrived after switching away from tab 1, however new its
    // generation, must be dropped.
    let stale = ParseResult {
        tab: 1,
        generation: 9,
        ..ParseResult::default()
    };
    assert!(
        !accepts_tab_latest(active_tab, 0, stale.tab, stale.generation),
        "a parse for a tab that is no longer active must be rejected"
    );

    // The active tab's own parse is accepted.
    let current = ParseResult {
        tab: active_tab,
        generation: 1,
        ..ParseResult::default()
    };
    assert!(accepts_tab_latest(
        active_tab,
        0,
        current.tab,
        current.generation
    ));

    // An older result from the active tab is also stale and rejected.
    assert!(
        !accepts_tab_latest(active_tab, 2, current.tab, current.generation),
        "a parse older than the applied generation must be rejected"
    );
}

#[test]
fn export_slot_refuses_a_second_export_until_it_finishes() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut export = ExportState {
        rx,
        tx,
        png_dialog: false,
        exporting: false,
    };

    assert!(export.begin_export(), "the first export claims the slot");
    assert!(
        !export.begin_export(),
        "a second export must be refused while one is in flight"
    );

    export.finish_export();
    assert!(
        export.begin_export(),
        "draining the result releases the slot for the next export"
    );
}

#[test]
fn clamp_caret_snaps_to_char_boundary_and_clamps_to_length() {
    let text = "aé";
    assert_eq!(clamp_caret(text, 0), 0);
    assert_eq!(clamp_caret(text, 1), 1);
    assert_eq!(clamp_caret(text, 2), 1, "a mid-char offset snaps down");
    assert_eq!(clamp_caret(text, 3), 3);
    assert_eq!(
        clamp_caret(text, 99),
        3,
        "past the end clamps to the length"
    );
}

#[test]
fn reroot_path_rewrites_equal_and_descendant_paths() {
    let old = Path::new("/w/old");
    let new = Path::new("/w/new");
    assert_eq!(
        reroot_path(Path::new("/w/old"), old, new),
        Some(PathBuf::from("/w/new"))
    );
    assert_eq!(
        reroot_path(Path::new("/w/old/chapters/a.typ"), old, new),
        Some(PathBuf::from("/w/new/chapters/a.typ"))
    );
    assert_eq!(reroot_path(Path::new("/w/older"), old, new), None);
    assert_eq!(reroot_path(Path::new("/w/other.typ"), old, new), None);
}

#[test]
fn dirty_tabs_under_matches_descendants_only_when_dirty() {
    let tabs = vec![
        tab(Some("/w/docs/main.typ"), true),
        tab(Some("/w/docs/chapters/a.typ"), false),
        tab(Some("/w/docs/other.typ"), true),
        tab(Some("/w/notes/b.typ"), true),
        tab(None, true),
    ];
    assert_eq!(
        dirty_tabs_under(&tabs, Path::new("/w/docs")),
        ["main.typ", "other.typ"]
    );
    assert_eq!(dirty_tabs_under(&tabs, Path::new("/w/notes")), ["b.typ"]);
    assert!(dirty_tabs_under(&tabs, Path::new("/w/missing")).is_empty());
}

fn diag_row(message: &str, severity: typ_edit_model::Severity) -> ErrorRow {
    ErrorRow {
        location: "doc.typ:1:1".to_owned(),
        message: message.to_owned(),
        path: PathBuf::from("doc.typ"),
        offset: 0,
        severity,
    }
}

#[test]
fn first_error_message_prefers_an_error_diagnostic_then_the_fatal_error() {
    use typ_edit_model::Severity;

    let warnings = [diag_row("warn", Severity::Warning)];
    assert_eq!(
        first_error_message(&warnings, &Some("fatal".to_owned())),
        "fatal"
    );

    let errors = [
        diag_row("warn", Severity::Warning),
        diag_row("boom", Severity::Error),
    ];
    assert_eq!(
        first_error_message(&errors, &Some("fatal".to_owned())),
        "boom"
    );

    assert_eq!(first_error_message(&[], &None), "编译失败");
}

#[test]
fn hover_due_requires_dwell_stillness_and_no_suppression() {
    assert!(
        !hover_due(Duration::from_millis(299), 0.0, false),
        "a shorter dwell must not show the tooltip"
    );
    assert!(hover_due(Duration::from_millis(300), 0.0, false));
    assert!(hover_due(Duration::from_millis(1200), 1.5, false));
    assert!(
        !hover_due(Duration::from_millis(1200), 3.1, false),
        "drift beyond the tolerance restarts the dwell"
    );
    assert!(hover_due(Duration::from_millis(1200), 3.0, false));
    assert!(
        !hover_due(Duration::from_millis(1200), 0.0, true),
        "Escape keeps the tooltip hidden"
    );
}

#[test]
fn follow_due_waits_for_quiet_and_never_fires_while_compiling() {
    let debounce = Duration::from_millis(200);
    assert!(
        !follow_due(Duration::from_millis(199), false, debounce),
        "a shorter quiet period must not fire"
    );
    assert!(follow_due(Duration::from_millis(200), false, debounce));
    assert!(follow_due(Duration::from_millis(900), false, debounce));
    assert!(
        !follow_due(Duration::from_millis(900), true, debounce),
        "an in-flight compile defers the follow to its result"
    );
}

/// File name of the path some `unique_image_path` call produced.
fn picked_name(path: Option<PathBuf>) -> Option<String> {
    path.and_then(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    })
}

#[test]
fn unique_image_path_skips_existing_files() {
    let dir = scratch_dir("image-names");
    let name = |stem: &str, ext: &str| picked_name(unique_image_path(&dir, stem, ext));
    assert_eq!(name("pasted", "png"), Some("pasted-1.png".to_owned()));
    std::fs::write(dir.join("pasted-1.png"), b"x").expect("write");
    assert_eq!(name("pasted", "png"), Some("pasted-2.png".to_owned()));
    std::fs::write(dir.join("pasted-2.png"), b"x").expect("write");
    assert_eq!(name("pasted", "png"), Some("pasted-3.png".to_owned()));
    // The dropped-image scheme keeps the source extension verbatim.
    assert_eq!(name("dropped", "JPG"), Some("dropped-1.JPG".to_owned()));
    assert_eq!(unique_image_path(&dir, "", "png"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn is_supported_image_matches_extensions_case_insensitively() {
    for good in [
        "a.png", "B.PNG", "c.jpg", "d.JPEG", "e.gif", "f.svg", "g.webp", "h.BMP",
    ] {
        assert!(
            is_supported_image(Path::new(good)),
            "{good} should be supported"
        );
    }
    for bad in ["a.typ", "b.txt", "c", "d.pdf", "e.png.bak"] {
        assert!(
            !is_supported_image(Path::new(bad)),
            "{bad} should be unsupported"
        );
    }
}

#[test]
fn relative_snippet_path_walks_up_and_uses_forward_slashes() {
    assert_eq!(
        relative_snippet_path(
            Path::new("/proj/doc"),
            Path::new("/proj/image/pasted-1.png")
        ),
        Some("../image/pasted-1.png".to_owned())
    );
    assert_eq!(
        relative_snippet_path(Path::new("/proj"), Path::new("/proj/image/pasted-1.png")),
        Some("image/pasted-1.png".to_owned())
    );
    assert_eq!(
        relative_snippet_path(Path::new("/a/b"), Path::new("/a/c/x.png")),
        Some("../c/x.png".to_owned())
    );
    assert_eq!(
        relative_snippet_path(Path::new("a"), Path::new("b/c.png")),
        None
    );
}

#[test]
fn references_in_place_only_for_paths_inside_the_workspace_root() {
    let root = Path::new("/w/proj");
    assert!(references_in_place(Path::new("/w/proj/a.png"), Some(root)));
    assert!(references_in_place(
        Path::new("/w/proj/img/a.png"),
        Some(root)
    ));
    assert!(
        !references_in_place(Path::new("/w/other/a.png"), Some(root)),
        "a path outside the root is copied"
    );
    assert!(
        !references_in_place(Path::new("/w/project/a.png"), Some(root)),
        "a sibling sharing only a text prefix is not inside the root"
    );
    assert!(
        !references_in_place(Path::new("/w/proj/a.png"), None),
        "without a workspace root nothing can be referenced in place"
    );
}

#[test]
fn in_place_reference_snippet_is_relative_to_the_active_file() {
    let root = Path::new("/w/proj");
    let source = Path::new("/w/proj/assets/logo.png");
    assert!(references_in_place(source, Some(root)));
    assert_eq!(
        relative_snippet_path(Path::new("/w/proj/chapters"), source),
        Some("../assets/logo.png".to_owned()),
        "an in-place image keeps its original name, made relative to the active file"
    );
}

#[test]
fn combined_snippets_join_a_batch_into_one_insert() {
    let snippets = vec![
        "#image(\"a.png\")".to_owned(),
        "#image(\"../img/b.png\")".to_owned(),
    ];
    assert_eq!(
        combined_snippets(&snippets),
        "#image(\"a.png\")\n#image(\"../img/b.png\")"
    );
    assert_eq!(combined_snippets(&snippets[..1]), "#image(\"a.png\")");
    assert_eq!(combined_snippets(&[]), "");
}

#[test]
fn editor_gate_waits_for_the_first_parse_of_a_large_buffer() {
    let mut gate = EditorGate::default();
    let large = LARGE_OPEN_GATE_BYTES + 1;
    gate.arm(7, large);
    assert!(gate.is_pending(large));

    assert!(
        !gate.observe(6),
        "a parse older than the awaited one must not clear the gate"
    );
    assert!(gate.is_pending(large));

    assert!(gate.observe(7), "the awaited parse clears the gate");
    assert!(!gate.is_pending(large));
    assert!(gate.take_focus(), "clearing arms one focus request");
    assert!(!gate.take_focus(), "the focus request is consumed once");
}

#[test]
fn editor_gate_request_focus_then_take_focus() {
    let mut gate = EditorGate::default();
    assert!(
        !gate.take_focus(),
        "a fresh gate has no focus request to take"
    );

    gate.request_focus();
    assert!(
        gate.take_focus(),
        "request_focus arms exactly one request for the next frame"
    );
    assert!(
        !gate.take_focus(),
        "take_focus consumes the request so it cannot repeat"
    );
}

#[test]
fn editor_gate_never_arms_for_small_buffers() {
    let mut gate = EditorGate::default();
    gate.arm(3, LARGE_OPEN_GATE_BYTES);
    assert!(!gate.is_pending(LARGE_OPEN_GATE_BYTES));
    gate.arm(4, LARGE_OPEN_GATE_BYTES - 1);
    assert!(!gate.is_pending(LARGE_OPEN_GATE_BYTES - 1));
    assert!(!gate.observe(4), "a disarmed gate never defers a parse");
    assert!(!gate.take_focus(), "a disarmed gate never arms focus");
}

#[test]
fn resize_direction_finds_edges_and_bottom_corners() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    let zone = |x: f32, y: f32| resize_direction(egui::pos2(x, y), rect, 6.0, TITLE_BAR_H);

    assert_eq!(zone(1.0, 300.0), Some(egui::ResizeDirection::West));
    assert_eq!(zone(799.0, 300.0), Some(egui::ResizeDirection::East));
    assert_eq!(zone(400.0, 599.0), Some(egui::ResizeDirection::South));
    assert_eq!(
        zone(1.0, 599.0),
        Some(egui::ResizeDirection::SouthWest),
        "a bottom corner takes priority over both edges"
    );
    assert_eq!(
        zone(799.0, 599.0),
        Some(egui::ResizeDirection::SouthEast),
        "the bottom-right corner resizes both axes"
    );
    assert_eq!(
        zone(6.1, 593.9),
        None,
        "just inside the west/south margins is not a resize zone"
    );
}

#[test]
fn resize_direction_ignores_the_title_strip_and_the_interior() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    let zone = |x: f32, y: f32| resize_direction(egui::pos2(x, y), rect, 6.0, TITLE_BAR_H);

    assert_eq!(
        zone(1.0, TITLE_BAR_H - 1.0),
        None,
        "the title strip excludes even the west edge"
    );
    assert_eq!(
        zone(1.0, TITLE_BAR_H),
        Some(egui::ResizeDirection::West),
        "the west edge starts exactly below the strip"
    );
    assert_eq!(
        zone(400.0, TITLE_BAR_H + 40.0),
        None,
        "the window centre is not a resize zone"
    );
    assert_eq!(zone(400.0, 0.0), None, "there is no north resize zone");
}

/// Empty mode sets, for classifier positions where only `text` matters.
const NO_RANGES: &[Range<usize>] = &[];

#[test]
fn decide_insertion_rejects_positions_adjacent_to_an_identifier() {
    for marked in ["#tab|le", "#align(c|enter,[])", "#s|et"] {
        let byte = marked.find('|').expect("caret marker");
        let text = marked.replacen('|', "", 1);
        let code = std::iter::once(0..text.len()).collect::<Vec<_>>();
        assert_eq!(
            decide_insertion(&text, byte, &code, NO_RANGES, NO_RANGES),
            InsertDecision::Reject(RejectReason::AdjacentIdentifier),
            "{marked:?}"
        );
    }
}

#[test]
fn decide_insertion_rejects_inside_math() {
    let marked = "$integra|l$";
    let byte = marked.find('|').expect("caret marker");
    let text = marked.replacen('|', "", 1);
    let math = std::iter::once(0..text.len()).collect::<Vec<_>>();
    assert_eq!(
        decide_insertion(&text, byte, NO_RANGES, &math, NO_RANGES),
        InsertDecision::Reject(RejectReason::Math)
    );

    // The caret between two spaces is not next to an identifier, yet math wins.
    let text = "$ x $";
    let math = std::iter::once(0..text.len()).collect::<Vec<_>>();
    assert_eq!(
        decide_insertion(text, 4, NO_RANGES, &math, NO_RANGES),
        InsertDecision::Reject(RejectReason::Math)
    );
}

#[test]
fn decide_insertion_rejects_inside_blocked_raw_or_comment() {
    let marked = "```\nraw |\n```";
    let byte = marked.find('|').expect("caret marker");
    let text = marked.replacen('|', "", 1);
    let blocked = std::iter::once(0..text.len()).collect::<Vec<_>>();
    assert_eq!(
        decide_insertion(&text, byte, NO_RANGES, NO_RANGES, &blocked),
        InsertDecision::Reject(RejectReason::Blocked)
    );

    let marked = "text // a |comment";
    let byte = marked.find('|').expect("caret marker");
    let text = marked.replacen('|', "", 1);
    let comment_start = text.find("//").expect("comment marker");
    let blocked = std::iter::once(comment_start..text.len()).collect::<Vec<_>>();
    assert_eq!(
        decide_insertion(&text, byte, NO_RANGES, NO_RANGES, &blocked),
        InsertDecision::Reject(RejectReason::Blocked)
    );
}

#[test]
fn decide_insertion_selects_markup_or_code() {
    // A plain markup position at the end of a paragraph.
    assert_eq!(
        decide_insertion("= Title\n\n", 9, NO_RANGES, NO_RANGES, NO_RANGES),
        InsertDecision::Insert(ImageForm::Markup)
    );

    // Between `=` and the space that follows it, inside the code range.
    let text = "#let x = 1";
    let code = std::iter::once(0..text.len()).collect::<Vec<_>>();
    assert_eq!(
        decide_insertion(text, 8, &code, NO_RANGES, NO_RANGES),
        InsertDecision::Insert(ImageForm::Code)
    );
}

#[test]
fn decide_insertion_handles_empty_text_and_bad_boundaries() {
    assert_eq!(
        decide_insertion("", 0, NO_RANGES, NO_RANGES, NO_RANGES),
        InsertDecision::Insert(ImageForm::Markup)
    );

    // A byte past the end clamps to the trailing space, keeping markup form.
    assert_eq!(
        decide_insertion("hi ", 999, NO_RANGES, NO_RANGES, NO_RANGES),
        InsertDecision::Insert(ImageForm::Markup)
    );

    // A byte inside the 3-byte `中` snaps to the nearer boundary (the `a`),
    // whose left neighbour is an identifier character.
    assert_eq!(
        decide_insertion("a中b", 2, NO_RANGES, NO_RANGES, NO_RANGES),
        InsertDecision::Reject(RejectReason::AdjacentIdentifier)
    );
}

// ---- rename: pure edit helpers --------------------------------------------

fn edit(range: Range<usize>, text: &str) -> TextEdit {
    TextEdit {
        range: BytePos(range.start)..BytePos(range.end),
        new_text: text.to_owned(),
    }
}

#[test]
fn apply_edits_to_text_applies_edits_back_to_front() {
    let edits = [edit(0..3, "baz"), edit(8..11, "baz")];
    assert_eq!(apply_edits_to_text("foo bar foo", &edits), "baz bar baz");
}

#[test]
fn apply_edits_to_text_is_multibyte_safe() {
    // '世' = bytes 6..9, so only a boundary-aligned range may be replaced.
    assert_eq!(
        apply_edits_to_text("你好世界", &[edit(6..9, "X")]),
        "你好X界"
    );
}

#[test]
fn apply_edits_to_text_skips_overlapping_and_out_of_range() {
    let edits = [
        edit(0..3, "X"),
        edit(2..5, "Y"),
        edit(4..99, "Z"),
        edit(100..101, "W"),
    ];
    assert_eq!(
        apply_edits_to_text("abcdef", &edits),
        "XdZ",
        "the overlap and the past-the-end edit are skipped"
    );
}

#[test]
fn apply_edits_to_text_is_a_noop_on_empty_input() {
    assert_eq!(apply_edits_to_text("abc", &[]), "abc");
    assert_eq!(apply_edits_to_text("abc", &[edit(1..1, "")]), "abc");
}

#[test]
fn edit_counts_reports_total_edits_and_distinct_files() {
    let files = [
        FileEdits {
            path: PathBuf::from("a.typ"),
            edits: vec![edit(0..1, "x"), edit(4..5, "x")],
        },
        FileEdits {
            path: PathBuf::from("b.typ"),
            edits: vec![edit(2..3, "x")],
        },
    ];
    assert_eq!(edit_counts(&files), (3, 2));
    assert_eq!(edit_counts(&[]), (0, 0));
}

#[test]
fn completion_answers_are_current_only_for_the_latest_document() {
    assert!(accepts_exact_current(Some(3), 3));
    assert!(!accepts_exact_current(Some(4), 3));
    assert!(
        !accepts_exact_current(None, 3),
        "a document that vanished makes every answer stale"
    );
}

#[test]
fn stored_member_completions_are_dropped_once_the_caret_moves() {
    assert!(!completions_moved_away(5, 5), "same caret keeps the list");
    assert!(completions_moved_away(5, 6), "a moved caret drops the list");
    assert!(completions_moved_away(5, 4));
}

#[test]
fn settings_persist_due_waits_out_the_debounce() {
    let now = Instant::now();
    assert!(
        !settings_persist_due(
            true,
            Some(now),
            now + Duration::from_millis(799),
            SETTINGS_PERSIST_DEBOUNCE
        ),
        "a shorter idle must not be due"
    );
    assert!(settings_persist_due(
        true,
        Some(now),
        now + SETTINGS_PERSIST_DEBOUNCE,
        SETTINGS_PERSIST_DEBOUNCE
    ));
    assert!(settings_persist_due(
        true,
        Some(now),
        now + Duration::from_millis(5000),
        SETTINGS_PERSIST_DEBOUNCE
    ));
}

#[test]
fn settings_persist_due_is_false_when_clean_and_true_without_a_stamp() {
    let now = Instant::now();
    assert!(!settings_persist_due(
        false,
        Some(now),
        now + SETTINGS_PERSIST_DEBOUNCE,
        SETTINGS_PERSIST_DEBOUNCE
    ));
    assert!(!settings_persist_due(
        false,
        None,
        now,
        SETTINGS_PERSIST_DEBOUNCE
    ));
    assert!(
        settings_persist_due(true, None, now, SETTINGS_PERSIST_DEBOUNCE),
        "a pending change with no stamp is due at once"
    );
}

#[test]
fn collab_command_predicates_gate_on_session_kind() {
    use super::collab::SessionKind;
    use super::dispatch::{
        collab_client_enabled, collab_join_enabled, collab_server_enabled, collab_share_enabled,
    };

    assert!(collab_share_enabled(SessionKind::Off, true));
    assert!(
        !collab_share_enabled(SessionKind::Off, false),
        "share needs a workspace"
    );
    assert!(!collab_share_enabled(SessionKind::Server, true));
    assert!(!collab_share_enabled(SessionKind::Client, true));

    assert!(collab_join_enabled(SessionKind::Off));
    assert!(!collab_join_enabled(SessionKind::Server));
    assert!(!collab_join_enabled(SessionKind::Client));

    assert!(collab_server_enabled(SessionKind::Server));
    assert!(!collab_server_enabled(SessionKind::Off));
    assert!(!collab_server_enabled(SessionKind::Client));

    assert!(collab_client_enabled(SessionKind::Client));
    assert!(!collab_client_enabled(SessionKind::Off));
    assert!(!collab_client_enabled(SessionKind::Server));
}

#[test]
fn collab_controller_starts_off_and_toggles_dialogs() {
    use super::collab::{CollabController, SessionKind};
    use crate::settings::Settings;

    let mut settings = Settings::default();
    settings.collab.username = "ann".to_owned();
    settings.collab.port = 40000;

    let mut controller = CollabController::default();
    assert_eq!(controller.kind(), SessionKind::Off);
    assert!(!controller.is_active());

    controller.open_share_dialog(&settings);
    assert!(controller.share_open);
    assert_eq!(controller.share_username, "ann");
    assert_eq!(controller.share_port, "40000");
    controller.close_share();
    assert!(!controller.share_open);

    // Stop/disconnect on an idle controller are no-ops.
    controller.stop_server();
    controller.disconnect();
    assert_eq!(controller.kind(), SessionKind::Off);
    assert!(!controller.is_active());
}
