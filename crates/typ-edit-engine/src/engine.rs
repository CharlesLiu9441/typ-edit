//! Background workers: parsing, compilation, raster preview rendering, spell
//! checking, project-wide search and IDE queries.
//!
//! This channel plumbing is a FROZEN CONTRACT for the app: every `request_*`
//! method and every [`EngineEvents`] field keeps its shape, and workers are only
//! reached through it. Requests travel over a latest-wins single-slot mailbox
//! ([`crate::latest`]); results return on small bounded channels and wake the UI.
//! Each worker runs its session loop under [`crate::supervisor`], so a panic is
//! isolated and reported through [`EngineHealth`] instead of killing the editor.
//! The worker implementations live in [`parse`](crate::parse),
//! [`compile`](crate::compile), [`raster`](crate::raster), [`spell`](crate::spell),
//! [`search`](crate::search) and [`ide`](crate::ide).

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::fonts::Fonts;
use crate::ide::IdeWork;
use crate::latest::{LatestSender, latest};
use crate::results::{CompileResult, PreviewDoc};
use crate::spell::{SpellMsg, merge_spell_request};
use crate::supervisor::EngineHealth;
use crate::wake::Waker;
use crossbeam_channel::{Receiver, bounded};
use typ_edit_model::{
    CompileRequest, IdeQuery, IdeRequest, IdeResult, ParseRequest, ParseResult,
    ProjectSearchRequest, ProjectSearchResult, RasterRequest, RasterResult, SpellRequest,
    SpellResult,
};

/// How long [`Engine::shutdown`] waits for the workers before detaching them.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(2);

/// How often shutdown polls the worker handles while waiting for the deadline.
const SHUTDOWN_POLL: Duration = Duration::from_millis(5);

// Result-channel bounds. Compile and raster keep only the newest result alive, so
// a depth of two absorbs an in-flight overwrite; the others keep a small buffer.
const PARSE_RESULT_BOUND: usize = 16;
const COMPILE_RESULT_BOUND: usize = 2;
const RASTER_RESULT_BOUND: usize = 2;
const SPELL_RESULT_BOUND: usize = 16;
const SEARCH_RESULT_BOUND: usize = 16;
const IDE_RESULT_BOUND: usize = 16;
const HEALTH_BOUND: usize = 32;

/// Handle to all background workers. Lives on the UI thread.
pub struct Engine {
    parse_tx: Option<LatestSender<ParseRequest>>,
    compile_tx: Option<LatestSender<CompileRequest>>,
    preview_doc_tx: Option<LatestSender<PreviewDoc>>,
    raster_tx: Option<LatestSender<RasterRequest>>,
    spell_tx: Option<LatestSender<SpellMsg>>,
    search_tx: Option<LatestSender<ProjectSearchRequest>>,
    ide_tx: Option<LatestSender<IdeWork>>,
    parse_rx: Option<Receiver<ParseResult>>,
    compile_rx: Option<Receiver<CompileResult>>,
    raster_rx: Option<Receiver<RasterResult>>,
    spell_rx: Option<Receiver<SpellResult>>,
    search_rx: Option<Receiver<ProjectSearchResult>>,
    ide_rx: Option<Receiver<IdeResult>>,
    health_rx: Option<Receiver<EngineHealth>>,
    handles: Vec<JoinHandle<()>>,
}

/// All worker messages drained in one UI frame.
#[derive(Default)]
pub struct EngineEvents {
    pub parses: Vec<ParseResult>,
    pub compiles: Vec<CompileResult>,
    pub raster: Vec<RasterResult>,
    pub spells: Vec<SpellResult>,
    pub searches: Vec<ProjectSearchResult>,
    pub ide: Vec<IdeResult>,
    /// Worker health events (restarts / stops / task panics), drained last.
    pub health: Vec<EngineHealth>,
}

impl Engine {
    /// Spawns all worker threads. `waker` is used to wake the UI when results arrive.
    ///
    /// `fonts` is the single app-owned font catalog, scanned on a background
    /// thread and shared with the compile dispatcher; the dispatcher waits for
    /// the scan before its first compile.
    pub fn spawn(waker: Waker, fonts: Arc<Fonts>) -> Self {
        let (parse_tx, parse_req_rx) = latest();
        let (parse_res_tx, parse_res_rx) = bounded(PARSE_RESULT_BOUND);

        let (compile_tx, compile_req_rx) = latest();
        let (compile_res_tx, compile_res_rx) = bounded(COMPILE_RESULT_BOUND);

        let (preview_doc_tx, preview_doc_rx) = latest();
        let (raster_tx, raster_req_rx) = latest();
        let (raster_res_tx, raster_res_rx) = bounded(RASTER_RESULT_BOUND);

        let (spell_tx, spell_req_rx) = latest();
        let (spell_res_tx, spell_res_rx) = bounded(SPELL_RESULT_BOUND);

        let (search_tx, search_req_rx) = latest();
        let (search_res_tx, search_res_rx) = bounded(SEARCH_RESULT_BOUND);

        let (ide_tx, ide_req_rx) = latest();
        let (ide_res_tx, ide_res_rx) = bounded(IDE_RESULT_BOUND);

        let (health_tx, health_rx) = bounded(HEALTH_BOUND);

        let handles = vec![
            crate::parse::spawn(parse_req_rx, parse_res_tx, waker.clone(), health_tx.clone()),
            crate::compile::spawn(
                compile_req_rx,
                compile_res_tx,
                waker.clone(),
                fonts,
                health_tx.clone(),
            ),
            crate::raster::spawn(
                preview_doc_rx,
                raster_req_rx,
                raster_res_tx,
                waker.clone(),
                health_tx.clone(),
            ),
            crate::spell::spawn(spell_req_rx, spell_res_tx, waker.clone(), health_tx.clone()),
            crate::search::spawn(
                search_req_rx,
                search_res_tx,
                waker.clone(),
                health_tx.clone(),
            ),
            crate::ide::spawn(ide_req_rx, ide_res_tx, waker.clone(), health_tx.clone()),
        ];
        // The workers own the only live health senders; drop the extra clone so
        // `health_rx` disconnects once they exit.
        drop(health_tx);

        Self {
            parse_tx: Some(parse_tx),
            compile_tx: Some(compile_tx),
            preview_doc_tx: Some(preview_doc_tx),
            raster_tx: Some(raster_tx),
            spell_tx: Some(spell_tx),
            search_tx: Some(search_tx),
            ide_tx: Some(ide_tx),
            parse_rx: Some(parse_res_rx),
            compile_rx: Some(compile_res_rx),
            raster_rx: Some(raster_res_rx),
            spell_rx: Some(spell_res_rx),
            search_rx: Some(search_res_rx),
            ide_rx: Some(ide_res_rx),
            health_rx: Some(health_rx),
            handles,
        }
    }

    pub fn request_parse(&self, request: ParseRequest) {
        if let Some(sender) = &self.parse_tx {
            sender.send(request);
        }
    }

    pub fn request_compile(&self, request: CompileRequest) {
        if let Some(sender) = &self.compile_tx {
            sender.send(request);
        }
    }

    /// Replaces the document used by the raster worker.
    pub fn set_preview_doc(&self, doc: PreviewDoc) {
        if let Some(sender) = &self.preview_doc_tx {
            sender.send(doc);
        }
    }

    pub fn request_raster(&self, request: RasterRequest) {
        if let Some(sender) = &self.raster_tx {
            sender.send(request);
        }
    }

    /// Folds a spell request into the worker's pending slot. `Check` replaces the
    /// pending check; `Forget` cancels a pending same-tab check in send order.
    pub fn request_spell(&self, request: SpellRequest) {
        if let Some(sender) = &self.spell_tx {
            sender.update(|msg| merge_spell_request(msg, request));
        }
    }

    pub fn request_search(&self, request: ProjectSearchRequest) {
        if let Some(sender) = &self.search_tx {
            sender.send(request);
        }
    }

    /// Sends an IDE query answered against `result`'s compiled world. `root` is
    /// the project root, used to classify package files and enumerate sources.
    pub fn request_ide(&self, result: Arc<CompileResult>, root: PathBuf, query: IdeQuery) {
        let request = IdeRequest {
            generation: result.generation,
            root,
            query,
        };
        if let Some(sender) = &self.ide_tx {
            sender.send((request, result));
        }
    }

    /// Non-blocking drain of all worker channels. Health drains last.
    pub fn drain(&self) -> EngineEvents {
        let mut events = EngineEvents::default();
        if let Some(rx) = &self.parse_rx {
            while let Ok(m) = rx.try_recv() {
                events.parses.push(m);
            }
        }
        if let Some(rx) = &self.compile_rx {
            while let Ok(m) = rx.try_recv() {
                events.compiles.push(m);
            }
        }
        if let Some(rx) = &self.raster_rx {
            while let Ok(m) = rx.try_recv() {
                events.raster.push(m);
            }
        }
        if let Some(rx) = &self.spell_rx {
            while let Ok(m) = rx.try_recv() {
                events.spells.push(m);
            }
        }
        if let Some(rx) = &self.search_rx {
            while let Ok(m) = rx.try_recv() {
                events.searches.push(m);
            }
        }
        if let Some(rx) = &self.ide_rx {
            while let Ok(m) = rx.try_recv() {
                events.ide.push(m);
            }
        }
        if let Some(rx) = &self.health_rx {
            while let Ok(m) = rx.try_recv() {
                events.health.push(m);
            }
        }
        events
    }

    /// Stops every worker and joins them under one shared deadline.
    ///
    /// Dropping the request senders closes the workers' mailboxes (their
    /// `recv`/`select` sees disconnect); dropping the result receivers unblocks
    /// any worker parked in a blocking send. Finished handles are joined; whatever
    /// is still running at the deadline is logged and detached — those workers exit
    /// on their own once their current send fails. After this, `request_*` are
    /// no-ops and [`drain`](Self::drain) returns empty events.
    pub fn shutdown(&mut self) {
        self.parse_tx = None;
        self.compile_tx = None;
        self.preview_doc_tx = None;
        self.raster_tx = None;
        self.spell_tx = None;
        self.search_tx = None;
        self.ide_tx = None;

        self.parse_rx = None;
        self.compile_rx = None;
        self.raster_rx = None;
        self.spell_rx = None;
        self.search_rx = None;
        self.ide_rx = None;
        self.health_rx = None;

        let deadline = Instant::now() + SHUTDOWN_DEADLINE;
        let mut handles = std::mem::take(&mut self.handles);
        loop {
            let mut index = 0;
            while index < handles.len() {
                if handles[index].is_finished() {
                    let finished = handles.swap_remove(index);
                    let _ = finished.join();
                } else {
                    index += 1;
                }
            }
            if handles.is_empty() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(SHUTDOWN_POLL);
        }

        for handle in handles {
            log::warn!("engine worker still running at the shutdown deadline; detaching it");
            drop(handle);
        }
    }
}
