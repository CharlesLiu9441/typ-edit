//! Compile worker: build a [`WorldState`](super::world::WorldState), run `typst::compile`,
//! map diagnostics to byte ranges, report page count.
//!
//! The worker coalesces to the latest [`CompileRequest`]; the UI already debounces
//! edits, so this only absorbs bursts.

use std::any::Any;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::fonts::{FontCatalog, Fonts};
use crate::latest::LatestReceiver;
use crate::results::CompileResult;
use crate::supervisor::{EngineHealth, Pipeline, supervise};
use crate::wake::Waker;
use crate::world::WorldState;
use crossbeam_channel::{Sender, bounded, select};
use typ_edit_model::{BytePos, CompileRequest, Diag, Severity};
use typst::World;
use typst::diag::{SourceDiagnostic, Warned};
use typst::syntax::{DiagSpan, DiagSpanKind, FileId};
use typst_kit::packages::SystemPackages;
use typst_layout::PagedDocument;

/// One live compile; newer requests wait in a single latest-wins pending slot.
///
/// Typst compilation cannot be interrupted, so a second run would only keep a
/// superseded document alive and double the CPU/memory footprint. One run keeps
/// the large-document cost bounded; the newest request replaces `pending` and
/// starts as soon as the slot frees.
const MAX_CONCURRENT_COMPILES: usize = 1;

/// How many compile generations a global comemo entry may stay untouched.
///
/// `comemo::evict(max_age)` increments every entry's age and removes entries
/// whose age is then greater than `max_age`; a cache hit resets the age to zero.
/// With one eviction per compile, `1` keeps the just-finished version plus
/// anything hit since the previous compile, and drops everything older. `0`
/// would clear all eval/layout memoization after every compile and make the next
/// compile cold; `2+` retains another full document version (the probe observed
/// ~1.17 GB after three uncapped versions).
const COMEMO_MAX_AGE: usize = 1;

/// Bound on the dispatcher's completion mailbox: one message per run thread, with
/// room to absorb the few in flight.
const DONE_BOUND: usize = 8;

/// Long-lived handles shared by the dispatcher and every run thread it starts.
struct CompileShared {
    fonts: Arc<Fonts>,
    packages: Arc<SystemPackages>,
    res_tx: Sender<CompileResult>,
    waker: Waker,
    done_tx: Sender<u64>,
    health: Sender<EngineHealth>,
}

/// Spawns the compile dispatcher thread: reads the latest request, marks the
/// running compile superseded and keeps the newest request in a single
/// latest-wins pending slot. At most one `typst-compile-run` thread exists; a
/// new run starts only after the current one finishes.
///
/// The packages loader, completion channel and run bookkeeping are created once
/// and live outside the supervised session, so a dispatcher restart never drops a
/// completion channel or loses track of a live compile.
pub fn spawn(
    req: LatestReceiver<CompileRequest>,
    res_tx: Sender<CompileResult>,
    waker: Waker,
    fonts: Arc<Fonts>,
    health: Sender<EngineHealth>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("typst-compile".to_owned())
        .spawn(move || {
            // The app owns the single system-font catalog; this dispatcher and every
            // per-request compile thread share it instead of re-scanning.
            // Package locations do not change, so one loader serves every compile.
            let (done_tx, done_rx) = bounded::<u64>(DONE_BOUND);
            let shared = CompileShared {
                fonts,
                packages: Arc::new(crate::packages::system_packages()),
                res_tx,
                waker,
                done_tx,
                health,
            };

            // At most one compile runs at a time; one latest-wins request waits in
            // `pending`. `live` is the running compile, the only one whose result is
            // still deliverable; a newer request marks it superseded. This state
            // survives a restart, so an orphaned run cannot wedge `running` at the cap.
            let mut running = 0usize;
            let mut pending: Option<CompileRequest> = None;
            let mut live: Option<(u64, Arc<AtomicBool>)> = None;

            supervise(Pipeline::Compile, &shared.health, || {
                loop {
                    select! {
                        recv(req.tickle_rx()) -> msg => {
                            if msg.is_err() {
                                break;
                            }
                            let Some(request) = req.take() else {
                                continue;
                            };
                            let generation = request.generation;
                            if let Some((live_generation, token)) = &live
                                && !token.swap(true, Ordering::Relaxed) {
                                    log::debug!("compile gen={live_generation} superseded");
                                }
                            if running < MAX_CONCURRENT_COMPILES {
                                if let Some(token) = start_compile(request, &shared) {
                                    running += 1;
                                    live = Some((generation, token));
                                }
                            } else {
                                log::debug!("compile gen={generation} waits for a free slot");
                                pending = Some(request);
                            }
                        },
                        recv(done_rx) -> msg => {
                            let Ok(finished_generation) = msg else {
                                break;
                            };
                            running = running.saturating_sub(1);
                            if live.as_ref().is_some_and(|(live_generation, _)| {
                                *live_generation == finished_generation
                            }) {
                                live = None;
                            }
                            if let Some(request) = pending.take() {
                                let generation = request.generation;
                                if let Some(token) = start_compile(request, &shared) {
                                    running += 1;
                                    live = Some((generation, token));
                                }
                            }
                        },
                    }
                }
            });
        })
        .expect("spawn compile worker")
}

/// Starts one compile on a fresh thread; returns the token that supersedes it, or `None`
/// when the thread could not be spawned.
fn start_compile(request: CompileRequest, shared: &CompileShared) -> Option<Arc<AtomicBool>> {
    let generation = request.generation;
    let token = Arc::new(AtomicBool::new(false));
    let run_token = Arc::clone(&token);
    // The UI may reach the first compile before the background scan lands;
    // waiting here is on the compile dispatcher, never the UI thread.
    let fonts = shared.fonts.wait();
    let packages = Arc::clone(&shared.packages);
    let res_tx = shared.res_tx.clone();
    let waker = shared.waker.clone();
    let done_tx = shared.done_tx.clone();
    let health = shared.health.clone();
    log::debug!("compile gen={generation} start");

    let spawned = std::thread::Builder::new()
        .name("typst-compile-run".to_owned())
        .spawn(move || {
            let started = Instant::now();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(request, &fonts, packages)
            }));
            match result {
                Ok(result) => {
                    if run_token.load(Ordering::Relaxed) {
                        log::debug!(
                            "compile gen={generation} cancelled after {}ms (result discarded)",
                            started.elapsed().as_millis()
                        );
                    } else {
                        log::debug!(
                            "compile gen={generation} done in {}ms",
                            started.elapsed().as_millis()
                        );
                        // Wake only once the result is actually queued.
                        if res_tx.send(result).is_ok() {
                            waker.wake();
                        }
                    }
                }
                Err(payload) => {
                    let message = panic_message(payload.as_ref());
                    log::error!("compile gen={generation} panicked: {message}");
                    let _ = health.try_send(EngineHealth::TaskPanicked {
                        pipeline: Pipeline::Compile,
                        message: message.clone(),
                    });
                    // A panic must never leave the UI's in-flight compile hanging:
                    // publish a synthetic failure unless the job was superseded.
                    if !run_token.load(Ordering::Relaxed) {
                        let synthetic = CompileResult {
                            generation,
                            document: None,
                            world: None,
                            diagnostics: Vec::new(),
                            error: Some(format!("internal compile panic: {message}")),
                            page_count: 0,
                            missing: Vec::new(),
                        };
                        if res_tx.send(synthetic).is_ok() {
                            waker.wake();
                        }
                    }
                }
            }
            let _ = done_tx.send(generation);
        });

    match spawned {
        Ok(_) => Some(token),
        Err(err) => {
            log::error!("compile gen={generation} failed to spawn: {err}");
            None
        }
    }
}

/// Renders a caught panic payload as text, falling back to a placeholder.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

/// Compiles one request into a [`CompileResult`].
fn run(
    request: CompileRequest,
    fonts: &FontCatalog,
    packages: Arc<SystemPackages>,
) -> CompileResult {
    let generation = request.generation;
    let CompileRequest {
        root,
        main,
        overlay,
        ..
    } = request;
    // WorldState owns Strings; materialize the snapshots on this worker thread
    // so the UI never copies the document.
    let overlay = overlay
        .into_iter()
        .map(|(path, text)| (path, text.materialize()))
        .collect();
    // Source reuse is deliberately not attempted: CompileResult keeps this
    // WorldState alive for IDE queries, so a cached Source clone would make
    // Source::replace's Arc::make_mut deep-copy the whole syntax tree.
    let world = WorldState::new(root, main, overlay, fonts, packages);

    let Warned { output, warnings } = typst::compile::<PagedDocument>(&world);

    let mut diagnostics = map_diagnostics(&world, &warnings);
    let (document, error, page_count) = match output {
        Ok(document) => {
            let page_count = document.pages().len();
            (Some(Arc::new(document)), None, page_count)
        }
        Err(errors) => {
            diagnostics.extend(map_diagnostics(&world, &errors));
            (None, errors.first().map(|diag| diag.message.to_string()), 0)
        }
    };

    // World-dependent lints run after the compiler's own diagnostics: the
    // variable-font syntax check appends warnings, and the unknown-font
    // augmenter appends suggestions to the compiler's existing warnings. Both
    // inspect the main source, so the new warnings carry its path.
    if let Ok(source) = world.source(world.main()) {
        let mut variable_font = typ_edit_analysis::lint_font::variable_font_warnings(&source);
        let main_path = world.path_of(world.main()).unwrap_or_default();
        for diag in &mut variable_font {
            diag.path = main_path.clone();
        }
        diagnostics.extend(variable_font);
        typ_edit_analysis::lint_font::augment_unknown_font(
            &mut diagnostics,
            &source,
            fonts.families(),
        );
    }

    let missing = world.missing_paths();
    // All world queries for this version are done. Bound the global memoization
    // before returning so a long editing session cannot retain every compiled
    // version; the current version stays warm for a follow-up compile.
    typst::comemo::evict(COMEMO_MAX_AGE);
    CompileResult {
        generation,
        document,
        // The world is kept alive for source queries (jump_from_click / jump_from_cursor).
        world: Some(Arc::new(world)),
        diagnostics,
        error,
        page_count,
        missing,
    }
}

/// Maps compiler diagnostics to file + byte-range diagnostics.
fn map_diagnostics(world: &WorldState, diagnostics: &[SourceDiagnostic]) -> Vec<Diag> {
    diagnostics
        .iter()
        .filter_map(|diagnostic| {
            let (id, range) = span_range(world, diagnostic.span)?;
            Some(Diag {
                path: world.path_of(id).unwrap_or_default(),
                range: BytePos(range.start)..BytePos(range.end),
                message: diagnostic.message.to_string(),
                severity: match diagnostic.severity {
                    typst::diag::Severity::Error => Severity::Error,
                    typst::diag::Severity::Warning => Severity::Warning,
                },
            })
        })
        .collect()
}

/// Resolves a diagnostic span to the file and byte range it points at.
fn span_range(world: &WorldState, span: DiagSpan) -> Option<(FileId, Range<usize>)> {
    match span.get() {
        DiagSpanKind::Detached => None,
        DiagSpanKind::Number { id, .. } => {
            let source = world.source(id).ok()?;
            crate::parse::span_local_range(&source, span).map(|range| (id, range))
        }
        DiagSpanKind::Range { id, range } => Some((id, range)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::FontCatalog;
    use typ_edit_model::TextSnapshot;

    #[test]
    fn compile_concurrency_is_bounded_to_one_run() {
        assert_eq!(MAX_CONCURRENT_COMPILES, 1);
    }

    /// Compiling a tiny in-memory document yields a page and no diagnostics.
    #[test]
    fn compiles_a_minimal_document() {
        let fonts = FontCatalog::new();
        let root = std::env::temp_dir();
        let main = root.join("typ-edit-compile-test.typ");
        let request = CompileRequest {
            generation: 7,
            root,
            main,
            overlay: vec![(
                std::env::temp_dir().join("typ-edit-compile-test.typ"),
                TextSnapshot::from_owned("= Hello\n\nWorld.", 0),
            )],
        };

        let result = run(
            request,
            &fonts,
            Arc::new(crate::packages::system_packages()),
        );
        assert_eq!(result.generation, 7);
        assert!(
            result.error.is_none(),
            "unexpected error: {:?}",
            result.error
        );
        assert_eq!(result.page_count, 1);
        assert!(result.document.is_some());
        assert!(result.world.is_some());
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    }

    /// A second compile of the same document still succeeds after the first
    /// run evicts stale comemo entries.
    #[test]
    fn a_warm_recompile_survives_comemo_eviction() {
        let fonts = FontCatalog::new();
        let root = std::env::temp_dir();
        let main = root.join("typ-edit-compile-evict-test.typ");
        let packages = Arc::new(crate::packages::system_packages());
        let request = |generation| CompileRequest {
            generation,
            root: root.clone(),
            main: main.clone(),
            overlay: vec![(
                main.clone(),
                TextSnapshot::from_owned("= Cached\n\nBody.", 0),
            )],
        };

        let first = run(request(1), &fonts, Arc::clone(&packages));
        assert!(first.error.is_none(), "unexpected error: {:?}", first.error);
        assert_eq!(first.page_count, 1);

        let second = run(request(2), &fonts, packages);
        assert!(
            second.error.is_none(),
            "warm recompile failed: {:?}",
            second.error
        );
        assert_eq!(second.page_count, 1);
    }

    /// A type error is reported with a path and a byte range.
    #[test]
    fn reports_mapped_diagnostics() {
        let fonts = FontCatalog::new();
        let root = std::env::temp_dir();
        let main = root.join("typ-edit-compile-error-test.typ");
        let request = CompileRequest {
            generation: 1,
            root,
            main: main.clone(),
            overlay: vec![(
                main.clone(),
                TextSnapshot::from_owned("#let x = 1\n#(x.foo)\n", 0),
            )],
        };

        let result = run(
            request,
            &fonts,
            Arc::new(crate::packages::system_packages()),
        );
        assert!(result.error.is_some());
        assert_eq!(result.page_count, 0);
        assert!(!result.diagnostics.is_empty());
        let diag = &result.diagnostics[0];
        assert_eq!(diag.path, main);
        assert!(diag.range.start < diag.range.end);
    }

    #[test]
    fn reports_missing_project_files_for_the_host_to_fetch() {
        let fonts = FontCatalog::new();
        let root =
            std::env::temp_dir().join(format!("typ-edit-compile-missing-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create root");
        let main = root.join("main.typ");
        let request = CompileRequest {
            generation: 9,
            root: root.clone(),
            main: main.clone(),
            overlay: vec![(
                main,
                TextSnapshot::from_owned("#image(\"missing.png\")\n", 0),
            )],
        };

        let result = run(
            request,
            &fonts,
            Arc::new(crate::packages::system_packages()),
        );
        assert!(result.error.is_some(), "the image is absent");
        assert!(
            result.missing.iter().any(|path| path == "missing.png"),
            "missing={:?}",
            result.missing
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The variable-font syntax lint is merged into the compile diagnostics.
    #[test]
    fn variable_font_warnings_are_merged_into_compile_diagnostics() {
        let fonts = FontCatalog::new();
        let root = std::env::temp_dir();
        let main = root.join("typ-edit-compile-font-test.typ");
        let text = "#set text(font: \"SometingVF\")\nHello.";
        let request = CompileRequest {
            generation: 3,
            root,
            main: main.clone(),
            overlay: vec![(main.clone(), TextSnapshot::from_owned(text, 0))],
        };

        let result = run(
            request,
            &fonts,
            Arc::new(crate::packages::system_packages()),
        );

        let needle = "\"SometingVF\"";
        let start = text.find(needle).expect("font string in source");
        let warning = result
            .diagnostics
            .iter()
            .find(|diag| diag.message.starts_with("variable font is not supported"))
            .expect("variable-font warning");
        assert_eq!(
            warning.path, main,
            "the warning carries the main file's path"
        );
        assert_eq!(warning.range, BytePos(start)..BytePos(start + needle.len()));
        assert!(
            warning
                .message
                .contains("https://github.com/typst/typst/issues/185")
        );
    }

    /// An unknown-font diagnostic gains a hint when a similar family is
    /// installed. The typo is derived from a real family, so a machine without
    /// a suitable family simply skips the assertion.
    #[test]
    fn unknown_font_diagnostics_gain_similarity_hints() {
        let fonts = FontCatalog::new();
        let Some(typo) = fonts.families().iter().find_map(|family| {
            let mut chars = family.chars();
            chars.next_back()?;
            let typo: String = chars.collect();
            (!typo.is_empty() && !fonts.has_family(&typo)).then_some(typo)
        }) else {
            return;
        };

        let root = std::env::temp_dir();
        let main = root.join("typ-edit-compile-unknown-font-test.typ");
        let text = format!("#set text(font: \"{typo}\")\nHello.");
        let request = CompileRequest {
            generation: 4,
            root,
            main: main.clone(),
            overlay: vec![(main.clone(), TextSnapshot::from_owned(text, 0))],
        };

        let result = run(
            request,
            &fonts,
            Arc::new(crate::packages::system_packages()),
        );
        let Some(diag) = result
            .diagnostics
            .iter()
            .find(|diag| diag.message.starts_with("unknown font family:"))
        else {
            return;
        };
        assert!(
            diag.message.contains("\nHint: did you mean"),
            "a similar installed family should be suggested: {}",
            diag.message
        );
    }
}
