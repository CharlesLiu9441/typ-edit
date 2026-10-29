//! Headless benchmark of the costs an "open file" pays on the UI thread.
//!
//! Run: `cargo test -p typ-edit-editor --lib bench -- --ignored --nocapture`
//!
//! It is `#[ignore]`d so it never runs in the normal workspace gate (laying out
//! a 3.2 MB buffer shapes millions of glyphs and takes seconds).

use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use typ_edit_model::HighlightKind;

use crate::{EditorState, highlight, search, wrap};

/// Corpus files: the 3.2 MB / 30 k-line giant, a 5 k-line file and prose.
const CORPUS: [&str; 3] = ["giant_30k.typ", "giant_5k.typ", "prose.typ"];
const FAMILY: &str = "editor";
const FONT_SIZE: f32 = 14.0;
/// A typical editor text viewport width, in points.
const WRAP_WIDTH: f32 = 800.0;
const SYNTHETIC_SPANS: usize = 2000;

/// Corpus root, overridable with `TYP_EDIT_BENCH_CORPUS`.
fn corpus_dir() -> std::path::PathBuf {
    std::env::var_os("TYP_EDIT_BENCH_CORPUS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("typ-edit-bench-corpus"))
}

/// A headless context with the editor's named font family bound to the default
/// monospace faces. The real app binds the user's editor font; shaping cost
/// scales the same way, and this keeps the benchmark independent of the host's
/// installed fonts.
fn editor_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    let mut defs = egui::FontDefinitions::default();
    let mono = defs
        .families
        .get(&egui::FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    defs.families
        .insert(egui::FontFamily::Name(Arc::from(FAMILY)), mono);
    ctx.set_fonts(defs);
    // Fonts only become available after the first pass.
    ctx.run_ui(egui::RawInput::default(), |_| {})
        .drop_without_applying_deltas();
    ctx
}

/// Lays out `job`, returning the wall-clock time and the resulting galley.
fn layout(ctx: &egui::Context, job: egui::text::LayoutJob) -> (Duration, Arc<egui::Galley>) {
    let started = Instant::now();
    let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));
    (started.elapsed(), galley)
}

/// `count` short highlight spans spread evenly across `text`.
fn synthetic_spans(text: &str, count: usize) -> Vec<(Range<usize>, HighlightKind)> {
    let step = (text.len() / count.max(1)).max(1);
    let mut spans = Vec::with_capacity(count);
    let mut start = 0;
    while start < text.len() && spans.len() < count {
        let mut end = (start + 12).min(text.len());
        while end < text.len() && !text.is_char_boundary(end) {
            end += 1;
        }
        spans.push((start..end, HighlightKind::Keyword));
        start += step;
    }
    spans
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Best (least noisy) of `runs` samples.
fn best_of(runs: usize, mut sample: impl FnMut() -> Duration) -> Duration {
    (0..runs).map(|_| sample()).min().unwrap_or_default()
}

#[test]
#[ignore = "headless interactive-cost benchmark; run with --ignored --nocapture"]
fn interactive_costs() {
    let path = corpus_dir().join("giant_30k.typ");
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("missing {}", path.display());
        return;
    };
    let mut state = EditorState::new();
    state.set_text(text.clone());
    let end = text.len().saturating_sub(1);

    let started = Instant::now();
    for _ in 0..1000 {
        std::hint::black_box(state.line_column(end));
    }
    println!("line_column x1000       {:>9.3} ms", ms(started.elapsed()));

    let started = Instant::now();
    for _ in 0..3 {
        let _ = std::hint::black_box(search::find_matches(
            &text, "the", false, false, false, None,
        ));
    }
    println!("find_matches x3 (30k)   {:>9.3} ms", ms(started.elapsed()));
}

#[test]
#[ignore = "headless open-cost benchmark; run with --ignored --nocapture"]
fn open_cost_breakdown() {
    for corpus in CORPUS {
        let path = corpus_dir().join(corpus);
        let Ok(text) = std::fs::read_to_string(&path) else {
            println!("{corpus}: missing at {}", path.display());
            continue;
        };
        println!(
            "\n=== {corpus} ({} bytes, {} lines) ===",
            text.len(),
            text.lines().count()
        );

        let (parsed, timings) = typ_edit_engine::parse::run_timed(&text, 0, false);
        // Same comparable phase set as before the fusion: the old
        // stats/modes/definitions/list_lines phases are now the single
        // `visitor` walk, so `parse::run total` stays directly comparable.
        let parse_total = timings.parse
            + timings.visitor
            + timings.highlights
            + timings.diagnostics
            + timings.call_params;
        println!("parse::run total      {:>9.2} ms", ms(parse_total));
        println!("  Source::new         {:>9.2}", ms(timings.parse));
        println!("  visitor             {:>9.2}", ms(timings.visitor));
        println!("  highlights          {:>9.2}", ms(timings.highlights));
        println!("  diagnostics         {:>9.2}", ms(timings.diagnostics));
        println!("  call_params         {:>9.2}", ms(timings.call_params));
        println!("  folds               {:>9.2}", ms(timings.folds));
        println!("  symbols             {:>9.2}", ms(timings.symbols));
        println!("  prose               {:>9.2}", ms(timings.prose));
        println!(
            "  spans / definitions {} / {}",
            parsed.highlights.len(),
            parsed.ide.definitions.len()
        );

        let build_empty = best_of(5, || {
            let started = Instant::now();
            std::hint::black_box(highlight::build_layout_job_with_theme(
                &text,
                &[],
                FAMILY,
                FONT_SIZE,
                true,
            ));
            started.elapsed()
        });
        println!("build job (0 spans)   {:>9.2}", ms(build_empty));

        let spans = synthetic_spans(&text, SYNTHETIC_SPANS);
        let build_spans = best_of(5, || {
            let started = Instant::now();
            std::hint::black_box(highlight::build_layout_job_with_theme(
                &text, &spans, FAMILY, FONT_SIZE, true,
            ));
            started.elapsed()
        });
        println!(
            "build job ({:>4} spans){:>9.2}",
            spans.len(),
            ms(build_spans)
        );

        let ctx = editor_ctx();
        let empty_job = highlight::build_layout_job_with_theme(&text, &[], FAMILY, FONT_SIZE, true);
        let (first_empty, galley_empty) = layout(&ctx, empty_job);
        println!("layout first (0)      {:>9.2}", ms(first_empty));
        println!("  rows                {}", galley_empty.rows.len());

        // Same content again: egui's galley cache hit.
        let cached_job =
            highlight::build_layout_job_with_theme(&text, &[], FAMILY, FONT_SIZE, true);
        let (cached, _) = layout(&ctx, cached_job);
        println!("layout cached         {:>9.2}", ms(cached));

        // The parse result replaces the spans, so the job differs and egui
        // shapes the whole buffer a second time.
        let span_job =
            highlight::build_layout_job_with_theme(&text, &spans, FAMILY, FONT_SIZE, true);
        let (first_spans, _) = layout(&ctx, span_job);
        println!("layout spans (fresh)  {:>9.2}", ms(first_spans));

        let started = Instant::now();
        let wrapped =
            wrap::wrap_lines(&galley_empty, WRAP_WIDTH, 1.0, wrap::WrapContext::default());
        let wrap_time = started.elapsed();
        println!(
            "wrap_lines ({:>4}pt)  {:>9.2}  rows {} -> {}",
            WRAP_WIDTH as i64,
            ms(wrap_time),
            galley_empty.rows.len(),
            wrapped.rows.len()
        );

        // The status bar's `line_column` scans the whole buffer from byte 0 every
        // frame; this is that scan at the far end of the buffer.
        let started = Instant::now();
        let line_count = text.char_indices().filter(|(_, ch)| *ch == '\n').count() + 1;
        std::hint::black_box(line_count);
        let column = text
            .rsplit('\n')
            .next()
            .map_or(0, |line| line.chars().count());
        std::hint::black_box(column);
        println!("line_column scan      {:>9.2}", ms(started.elapsed()));

        let first_layout = ms(build_empty + first_empty);
        let open_ui = ms(build_empty + first_empty + build_spans + first_spans);
        println!("-------------------------------------------");
        println!("first layout only     {:>9.2} ms", first_layout);
        println!("open UI (2 layouts)   {:>9.2} ms", open_ui);
        println!(
            "parse + open UI       {:>9.2} ms",
            ms(parse_total) + open_ui
        );
    }
}
