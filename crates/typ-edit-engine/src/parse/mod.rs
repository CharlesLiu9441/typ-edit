//! Parse worker: typst syntax tree -> highlight spans, syntax diagnostics, counts.
//!
//! The worker coalesces to the latest [`ParseRequest`] so a burst of keystrokes costs
//! at most one parse; the editor re-renders from the next [`ParseResult`].

mod complete;
mod diag;
mod dict;
mod fold;
mod highlight;
mod incremental;
mod math_atoms;
mod params;
mod prose;
mod visit;

pub(crate) use complete::DefinedNames;
pub(crate) use diag::{diagnostics, lint_diagnostics, span_local_range};
pub(crate) use fold::fold_ranges;
pub(crate) use highlight::highlights;
pub(crate) use incremental::IncrementalParser;
pub(crate) use params::call_params;
pub use prose::prose_segments;
pub(crate) use visit::visit;

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::latest::LatestReceiver;
use crate::supervisor::{EngineHealth, Pipeline, supervise};
use crate::wake::Waker;
use crossbeam_channel::Sender;
use typ_edit_analysis::lexical;
use typ_edit_model::{CharCount, IdeData, LayoutHints, ParseRequest, ParseResult, TextStats};
use typst::syntax::{FileId, LinkedNode, RootedPath, Source, SyntaxKind, VirtualPath, VirtualRoot};

/// Spawns the parse worker thread. Reads a latest-wins mailbox and is supervised:
/// a panic restarts the session with a fresh [`IncrementalParser`].
pub(crate) fn spawn(
    req: LatestReceiver<ParseRequest>,
    res_tx: Sender<ParseResult>,
    waker: Waker,
    health: Sender<EngineHealth>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("typst-parse".to_owned())
        .spawn(move || {
            supervise(Pipeline::Parse, &health, || {
                // Derived, not durable: a restarted session re-parses from scratch.
                let mut parser = IncrementalParser::new();
                while let Ok(request) = req.recv() {
                    // Materialize on this worker thread, never the UI thread; the
                    // owned text moves into the parser's `Source` on first parse.
                    let text = request.text.materialize();
                    let result = parser.parse(text, request.cursor.0, request.want_spell);
                    let result = ParseResult {
                        generation: request.generation,
                        tab: request.tab,
                        ..result
                    };
                    if res_tx.send(result).is_err() {
                        break;
                    }
                    waker.wake();
                }
            });
        })
        .expect("spawn parse worker")
}

/// Parses `text` into a [`ParseResult`].
///
/// `cursor` is the caret byte offset; it resolves the call surrounding the
/// caret for parameter completion. The request's generation is stamped by
/// [`spawn`], which owns the request.
pub fn run(text: &str, cursor: usize) -> ParseResult {
    run_timed(text, cursor, false).0
}

/// Wall-clock cost of every phase of [`run_timed`], produced for the editor's
/// headless open-cost benchmark; the app workers call [`run`] and ignore it.
///
/// `visitor` replaces the standalone stats/modes/definitions/list-lines phases:
/// the fused [`visit`] produces all of them in one tree walk. `folds`, `symbols`
/// and `prose` are informational extras, outside the old comparable phase set.
#[derive(Clone, Copy, Debug, Default)]
pub struct ParseTimings {
    pub parse: Duration,
    pub visitor: Duration,
    pub highlights: Duration,
    pub diagnostics: Duration,
    pub call_params: Duration,
    pub folds: Duration,
    pub symbols: Duration,
    pub prose: Duration,
}

/// [`run`] with a per-phase wall-clock breakdown.
///
/// `want_spell` additionally extracts [`typ_edit_model::ProseSegment`]s; it is
/// `false` for [`run`], which has no spell consumer.
pub fn run_timed(text: &str, cursor: usize, want_spell: bool) -> (ParseResult, ParseTimings) {
    let mut timings = ParseTimings::default();

    // `Source::new` parses the text once; its tree is then used for both the
    // highlighting walk and the diagnostics.
    let started = Instant::now();
    let source = Source::new(parse_file_id(), text.to_owned());
    let root = LinkedNode::new(source.root());
    timings.parse = started.elapsed();

    // One fused walk produces stats, mode ranges, math atoms, list lines and
    // definitions.
    let started = Instant::now();
    let fused = visit(&root, source.text());
    timings.visitor = started.elapsed();

    // The highlight walk is parameterized by the definition set, so the same
    // set powers the full path and the incremental parser's splices.
    let defined = DefinedNames::from_items(&fused.definitions);
    let started = Instant::now();
    let highlights = highlight::typed(highlights(&root, &defined));
    timings.highlights = started.elapsed();

    let started = Instant::now();
    let mut diagnostics = diagnostics(&source);
    diagnostics.extend(lint_diagnostics(&source));
    timings.diagnostics = started.elapsed();

    let started = Instant::now();
    let call_params = call_params(&root, cursor);
    timings.call_params = started.elapsed();

    let started = Instant::now();
    let folds = fold_ranges(&source);
    timings.folds = started.elapsed();
    // The symbol outline is a cheap full walk with no extra consumer to gate on,
    // so it is always built.
    let started = Instant::now();
    let symbols = lexical::symbols(&source);
    timings.symbols = started.elapsed();
    // Prose extraction feeds only the spell worker; skip the walk entirely when
    // spell checking is off.
    let started = Instant::now();
    let prose = if want_spell {
        prose_segments(&source)
    } else {
        Vec::new()
    };
    timings.prose = started.elapsed();

    let result = ParseResult {
        generation: 0,
        tab: 0,
        highlights,
        diagnostics,
        stats: TextStats {
            words: fused.words,
            chars: CharCount(fused.chars),
        },
        layout: LayoutHints {
            blocked_ranges: fused.blocked_ranges,
            code_ranges: fused.code_ranges,
            math_ranges: fused.math_ranges,
            math_atoms: fused.math_atoms,
            list_lines: fused.list_lines,
        },
        ide: IdeData {
            definitions: fused.definitions,
            call_params,
        },
        folds,
        symbols,
        prose,
    };
    (result, timings)
}

/// The id used for the throwaway source of a parse request.
///
/// `FileId::new` interns globally and reuses an existing entry, so calling this for
/// every request does not leak ids.
fn parse_file_id() -> FileId {
    RootedPath::new(
        VirtualRoot::Project,
        VirtualPath::new("__typ-edit-parse__.typ").expect("valid literal path"),
    )
    .intern()
}

/// The syntax mode a byte position is in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Markup,
    Code,
    Math,
    Raw,
}

/// The syntax mode of `node` given its parent's mode: `Raw` is raw,
/// `Equation`/`Math` enter math, a `ContentBlock` outside markup re-enters
/// markup, and `Code`/`CodeBlock`/`#`-embedded expressions enter code.
fn node_mode(node: &LinkedNode<'_>, parent: Mode) -> Mode {
    let embedded_code = matches!(
        node.prev_sibling().map(|sibling| sibling.kind()),
        Some(SyntaxKind::Hash)
    );
    if node.kind() == SyntaxKind::Raw {
        Mode::Raw
    } else if matches!(node.kind(), SyntaxKind::Equation | SyntaxKind::Math) {
        Mode::Math
    } else if node.kind() == SyntaxKind::ContentBlock && parent != Mode::Markup {
        Mode::Markup
    } else if matches!(node.kind(), SyntaxKind::Code | SyntaxKind::CodeBlock) || embedded_code {
        Mode::Code
    } else {
        parent
    }
}

/// Upper bound on reported definitions keeps completion lists bounded.
const MAX_DEFINITIONS: usize = 1000;

#[cfg(test)]
mod let_tests;
#[cfg(test)]
mod tests;
