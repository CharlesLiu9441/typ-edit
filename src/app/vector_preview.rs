//! Vector preview backend: a per-page frame-BVH cache painted through
//! [`typ_edit_preview::VectorPagePainter`].
//!
//! The preview has exactly two paths: this vector backend at readable zooms and
//! the raster pipeline everywhere else (low zoom, no wgpu render state, an
//! adapter that cannot host the canvas render pass, or a page generation whose
//! GPU rendering failed). A whole-document failure records the reason so the
//! pane can show a fallback badge and surface a one-time toast.
//!
//! With the `gpu-canvas` feature the backend owns a real
//! [`GpuCanvasHandle`]; without it the handle type is uninhabited and the
//! preview always uses the raster path.

use super::state::DocResources;

#[cfg(feature = "gpu-canvas")]
use std::collections::{HashMap, HashSet};
#[cfg(feature = "gpu-canvas")]
use std::sync::mpsc::{Receiver, Sender, channel};
#[cfg(feature = "gpu-canvas")]
use std::sync::{Arc, Mutex};

#[cfg(feature = "gpu-canvas")]
use typ_edit_i18n::t;
#[cfg(feature = "gpu-canvas")]
use typst_gpu_canvas::{
    FrameBvh, GpuCanvas, PageFrameSource, PageRecording, RenderError, scale_bucket,
};
#[cfg(feature = "gpu-canvas")]
use typst_layout::PagedDocument;

/// The app's handle to the GPU canvas renderer.
///
/// With the `gpu-canvas` feature this aliases the real renderer; [`GpuCanvas::new`]
/// returns `None` for an adapter that cannot host the canvas render pass, so an
/// unsupported device falls back to raster exactly like a missing render state.
#[cfg(feature = "gpu-canvas")]
pub(crate) type GpuCanvasHandle = typst_gpu_canvas::GpuCanvas;

/// The app's handle to the GPU canvas renderer, absent without the `gpu-canvas`
/// feature: the type is uninhabited, so `Option<Self>` is always `None` and the
/// preview uses only the raster path.
#[cfg(not(feature = "gpu-canvas"))]
pub(crate) enum GpuCanvasHandle {}

/// One page recording key: document generation, page index, quantized zoom
/// bucket and pixels-per-point.
#[cfg(feature = "gpu-canvas")]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct RecordKey {
    generation: u64,
    page: usize,
    bucket_bits: u32,
    ppp_bits: u32,
    image_max_side: u32,
}

#[cfg(feature = "gpu-canvas")]
impl RecordKey {
    #[cfg(test)]
    fn new(generation: u64, page: usize, scale: f32, ppp: f32) -> Self {
        Self::with_image_max_side(
            generation,
            page,
            scale,
            ppp,
            crate::settings::GPU_IMAGE_MAX_SIDE_DEFAULT,
        )
    }

    fn with_image_max_side(
        generation: u64,
        page: usize,
        scale: f32,
        ppp: f32,
        image_max_side: u32,
    ) -> Self {
        Self {
            generation,
            page,
            bucket_bits: scale_bucket(scale).to_bits(),
            ppp_bits: ppp.to_bits(),
            image_max_side,
        }
    }

    fn bucket(&self) -> f32 {
        f32::from_bits(self.bucket_bits)
    }
}

#[cfg(feature = "gpu-canvas")]
struct RecordJob {
    key: RecordKey,
    page: usize,
    source: Arc<dyn PageFrameSource>,
    bvh: Option<Arc<FrameBvh>>,
    pos: (f32, f32),
    scale: f32,
    ppp: f32,
    ctx: egui::Context,
    max_texture_side: u32,
}

#[cfg(feature = "gpu-canvas")]
struct RecordReply {
    key: RecordKey,
    bvh: Option<Arc<FrameBvh>>,
    recording: Option<Arc<PageRecording>>,
    error: Option<RenderError>,
}

#[cfg(feature = "gpu-canvas")]
type Latest = Arc<Mutex<HashMap<(u64, usize), RecordKey>>>;

/// Starts one page-recording worker. The worker builds a page BVH and records
/// CPU commands; the UI only replays finished recordings.
#[cfg(feature = "gpu-canvas")]
fn recording_worker() -> (Sender<RecordJob>, Receiver<RecordReply>, Latest) {
    let (job_tx, job_rx) = channel::<RecordJob>();
    let (reply_tx, reply_rx) = channel::<RecordReply>();
    let latest: Latest = Arc::new(Mutex::new(HashMap::new()));
    let worker_latest = Arc::clone(&latest);
    let spawned = std::thread::Builder::new()
        .name("typ-edit-page-recorder".to_owned())
        .spawn(move || {
            while let Ok(job) = job_rx.recv() {
                let is_latest = worker_latest
                    .lock()
                    .ok()
                    .and_then(|map| map.get(&(job.key.generation, job.page)).copied())
                    == Some(job.key);
                if !is_latest {
                    continue;
                }
                let bvh = job.bvh.or_else(|| {
                    job.source
                        .frame(job.page)
                        .map(|frame| Arc::new(FrameBvh::new(frame)))
                });
                let Some(bvh) = bvh else {
                    continue;
                };
                let recorded = typst_gpu_canvas::record_page_with_image_max_side(
                    &bvh,
                    egui::pos2(job.pos.0, job.pos.1),
                    job.scale,
                    job.ppp,
                    &job.ctx,
                    job.max_texture_side,
                    job.key.image_max_side,
                );
                let (recording, error) = match recorded {
                    Ok(recording) => (Some(Arc::new(recording)), None),
                    Err(error) => (None, Some(error)),
                };
                if reply_tx
                    .send(RecordReply {
                        key: job.key,
                        bvh: Some(bvh),
                        recording,
                        error,
                    })
                    .is_err()
                {
                    break;
                }
                job.ctx.request_repaint();
            }
        });
    if let Err(err) = spawned {
        log::error!("cannot start page recording worker: {err}");
    }
    (job_tx, reply_rx, latest)
}

/// A cheap Arc-backed source for the page recorder. The worker clones a page's
/// frame only on its own thread.
#[cfg(feature = "gpu-canvas")]
struct DocPageFrames {
    document: Arc<PagedDocument>,
}

#[cfg(feature = "gpu-canvas")]
impl PageFrameSource for DocPageFrames {
    fn frame(&self, page: usize) -> Option<typst::layout::Frame> {
        self.document
            .pages()
            .get(page)
            .map(|paged| paged.frame.clone())
    }
}

/// Asynchronous page recordings plus the shared GPU canvas renderer.
///
/// Recordings are retained across compile generations: when a new document
/// arrives, the visible pages keep replaying the previous generation's
/// recording until their replacement has been recorded. The recorder worker
/// builds BVHs and records pages, while this struct only caches and replays.
#[cfg(feature = "gpu-canvas")]
pub(super) struct VectorPreview {
    generation: u64,
    previous_generation: Option<u64>,
    frames: Option<Arc<dyn PageFrameSource>>,
    gpu: Option<GpuCanvas>,
    image_max_side: u32,
    recordings: Vec<(RecordKey, Arc<PageRecording>)>,
    bvhs: Vec<((u64, usize), Arc<FrameBvh>)>,
    pending: HashSet<RecordKey>,
    wanted: HashMap<(u64, usize), RecordKey>,
    record_tx: Sender<RecordJob>,
    record_rx: Receiver<RecordReply>,
    latest: Latest,
    /// Generation whose document failed GPU rendering; the preview pane falls
    /// back to the raster pipeline for that whole document generation.
    failed_generation: Option<u64>,
    /// User-facing reason for the current failure (see [`render_error_label`]).
    failed_reason: Option<String>,
    /// Generation whose fallback notice has already been surfaced to the user.
    notified_generation: Option<u64>,
}

#[cfg(feature = "gpu-canvas")]
const MAX_RECORDINGS: usize = 32;
#[cfg(feature = "gpu-canvas")]
const MAX_BVHS: usize = 32;

#[cfg(feature = "gpu-canvas")]
impl VectorPreview {
    pub(super) fn new(gpu: Option<GpuCanvas>) -> Self {
        let (record_tx, record_rx, latest) = recording_worker();
        Self {
            generation: u64::MAX,
            previous_generation: None,
            frames: None,
            gpu,
            image_max_side: crate::settings::GPU_IMAGE_MAX_SIDE_DEFAULT,
            recordings: Vec::new(),
            bvhs: Vec::new(),
            pending: HashSet::new(),
            wanted: HashMap::new(),
            record_tx,
            record_rx,
            latest,
            failed_generation: None,
            failed_reason: None,
            notified_generation: None,
        }
    }

    pub(super) fn set_image_max_side(&mut self, image_max_side: u32) {
        let image_max_side = image_max_side.clamp(
            crate::settings::GPU_IMAGE_MAX_SIDE_MIN,
            crate::settings::GPU_IMAGE_MAX_SIDE_MAX,
        );
        if self.image_max_side == image_max_side {
            return;
        }
        self.image_max_side = image_max_side;
        if let Some(gpu) = &mut self.gpu {
            gpu.set_image_max_side(image_max_side);
        }
        self.recordings.clear();
        self.pending.clear();
        self.wanted.clear();
        if let Ok(mut latest) = self.latest.lock() {
            latest.clear();
        }
    }

    /// Whether GPU rendering failed for `generation`, so the preview must fall
    /// back to the raster pipeline until the document recompiles.
    pub(super) fn gpu_failed(&self, generation: u64) -> bool {
        self.failed_generation == Some(generation)
    }

    /// The user-facing reason for `generation`'s GPU fallback, if it failed.
    pub(super) fn fallback_reason(&self, generation: u64) -> Option<&str> {
        if self.gpu_failed(generation) {
            self.failed_reason.as_deref()
        } else {
            None
        }
    }

    /// Takes the one-time fallback notice for `generation`, if it has not been
    /// surfaced yet. Returns `None` for a generation that did not fail or that
    /// was already notified.
    pub(super) fn take_fallback_notice(&mut self, generation: u64) -> Option<String> {
        if !self.gpu_failed(generation) || self.notified_generation == Some(generation) {
            return None;
        }
        self.notified_generation = Some(generation);
        self.failed_reason.clone()
    }

    /// Borrows a per-frame painter against the current document. Old recordings
    /// are intentionally retained for the new generation's first frames.
    pub(super) fn painter<'a>(&'a mut self, doc: &'a DocResources) -> VectorPainter<'a> {
        if doc.generation != self.generation {
            if self.generation != u64::MAX {
                self.previous_generation = Some(self.generation);
            }
            self.generation = doc.generation;
            if let Some(gpu) = &self.gpu {
                gpu.set_document_generation(doc.generation);
            }
            self.wanted
                .retain(|(generation, _), _| *generation == doc.generation);
            self.pending
                .retain(|pending| pending.generation == doc.generation);
            // Keep only the immediately previous generation's in-flight jobs:
            // they can still provide its fallback, while anything older must not
            // be recorded or shown.
            self.retain_previous_jobs();
            let old = self.frames.replace(Arc::new(DocPageFrames {
                document: Arc::clone(&doc.document),
            }));
            if let Some(old) = old {
                super::background_drop::later(old);
            }
        }
        let frames = Arc::clone(
            self.frames
                .as_ref()
                .expect("painter installs a frame source"),
        );
        VectorPainter {
            generation: doc.generation,
            document: &doc.document,
            frames,
            preview: self,
        }
    }

    fn retain_previous_jobs(&self) {
        if let Ok(mut latest) = self.latest.lock() {
            latest.retain(|(generation, _), _| Some(*generation) == self.previous_generation);
        }
    }

    fn recording(&self, key: RecordKey) -> Option<Arc<PageRecording>> {
        self.recordings
            .iter()
            .find(|(cached, _)| *cached == key)
            .map(|(_, recording)| Arc::clone(recording))
    }

    fn bvh(&self, generation: u64, page: usize) -> Option<Arc<FrameBvh>> {
        self.bvhs
            .iter()
            .find(|(key, _)| *key == (generation, page))
            .map(|(_, bvh)| Arc::clone(bvh))
    }

    fn accepts_reply(&self, key: RecordKey) -> bool {
        (key.generation == self.generation || Some(key.generation) == self.previous_generation)
            && key.image_max_side == self.image_max_side
    }

    fn fallback_key(&self, page: usize, ppp: f32, key: RecordKey) -> Option<RecordKey> {
        self.recordings
            .iter()
            .filter(|(cached, _)| {
                cached.page == page
                    && cached.ppp_bits == ppp.to_bits()
                    && cached.image_max_side == self.image_max_side
                    && (cached.generation == key.generation
                        || Some(cached.generation) == self.previous_generation)
            })
            .map(|(cached, _)| *cached)
            .min_by_key(|cached| {
                let generation_rank = usize::from(cached.generation != key.generation);
                (
                    generation_rank,
                    cached.bucket_bits.abs_diff(key.bucket_bits),
                )
            })
    }

    fn fallback_recording(
        &self,
        page: usize,
        ppp: f32,
        key: RecordKey,
    ) -> Option<Arc<PageRecording>> {
        self.fallback_key(page, ppp, key)
            .and_then(|cached| self.recording(cached))
    }

    fn store_bvh(&mut self, key: (u64, usize), bvh: Arc<FrameBvh>) {
        self.bvhs.retain(|(cached, _)| *cached != key);
        self.bvhs.push((key, bvh));
        while self.bvhs.len() > MAX_BVHS {
            let Some(index) = self
                .bvhs
                .iter()
                .position(|(key, _)| !self.wanted.contains_key(key))
            else {
                break;
            };
            let (_, old) = self.bvhs.remove(index);
            super::background_drop::later(old);
        }
    }

    fn store_recording(&mut self, key: RecordKey, recording: Arc<PageRecording>) {
        self.recordings.retain(|(cached, _)| *cached != key);
        self.recordings.push((key, recording));
        while self.recordings.len() > MAX_RECORDINGS {
            let Some(index) = self
                .recordings
                .iter()
                .position(|(key, _)| self.wanted.get(&(key.generation, key.page)) != Some(key))
            else {
                break;
            };
            let (_, old) = self.recordings.remove(index);
            super::background_drop::later(old);
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(reply) = self.record_rx.try_recv() {
            if !self.accepts_reply(reply.key) {
                continue;
            }
            self.pending.remove(&reply.key);
            if let Some(bvh) = reply.bvh {
                self.store_bvh((reply.key.generation, reply.key.page), bvh);
            }
            if let Some(recording) = reply.recording {
                self.store_recording(reply.key, recording);
            }
            if let Some(error) = reply.error
                && self.wanted.get(&(reply.key.generation, reply.key.page)) == Some(&reply.key)
            {
                self.failed_generation = Some(reply.key.generation);
                self.failed_reason = Some(render_error_label(&error));
            }
        }
        if !self.pending.is_empty() {
            ctx.request_repaint();
        }
    }
}

#[cfg(feature = "gpu-canvas")]
impl Drop for VectorPreview {
    fn drop(&mut self) {
        if let Some(frames) = self.frames.take() {
            super::background_drop::later(frames);
        }
        for (_, recording) in self.recordings.drain(..) {
            super::background_drop::later(recording);
        }
        for (_, bvh) in self.bvhs.drain(..) {
            super::background_drop::later(bvh);
        }
    }
}

/// Paints preview pages through the GPU canvas.
#[cfg(feature = "gpu-canvas")]
pub(super) struct VectorPainter<'a> {
    generation: u64,
    document: &'a Arc<PagedDocument>,
    frames: Arc<dyn PageFrameSource>,
    preview: &'a mut VectorPreview,
}

#[cfg(feature = "gpu-canvas")]
impl typ_edit_preview::VectorPagePainter for VectorPainter<'_> {
    fn paint_page(
        &mut self,
        ui: &mut egui::Ui,
        page: usize,
        page_rect: egui::Rect,
        scale: f32,
    ) -> bool {
        let Some(paged) = self.document.pages().get(page) else {
            return false;
        };
        // The frame content covers only the page's items, while raster and SVG
        // targets also show the page's `fill_or_white` background; paint that
        // background first.
        match paged.fill_or_white() {
            Some(typst::visualize::Paint::Solid(color)) => {
                let [r, g, b, a] = color.to_vec4_u8();
                ui.painter().rect_filled(
                    page_rect,
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(r, g, b, a),
                );
            }
            Some(_) => {
                ui.painter()
                    .rect_filled(page_rect, 0.0, egui::Color32::WHITE);
            }
            None => {}
        }

        self.preview.poll(ui.ctx());
        let key = RecordKey::with_image_max_side(
            self.generation,
            page,
            scale,
            ui.ctx().pixels_per_point(),
            self.preview.image_max_side,
        );
        self.preview.wanted.insert((self.generation, page), key);
        let Some(gpu) = self.preview.gpu.as_ref() else {
            return false;
        };
        if let Some(recording) = self.preview.recording(key) {
            let _ = gpu.append_recording(ui, &recording, page_rect.min, scale);
            return true;
        }

        // Drop any older request for this page so a skipped worker job cannot
        // leave a permanent pending entry.
        self.preview.pending.retain(|pending| pending.page != page);
        if self.preview.pending.insert(key) {
            let job = RecordJob {
                key,
                page,
                source: Arc::clone(&self.frames),
                bvh: self.preview.bvh(self.generation, page),
                pos: (page_rect.min.x, page_rect.min.y),
                scale: key.bucket(),
                ppp: ui.ctx().pixels_per_point(),
                ctx: ui.ctx().clone(),
                max_texture_side: ui.ctx().input(|input| input.max_texture_side).max(1) as u32,
            };
            if let Ok(mut latest) = self.preview.latest.lock() {
                latest.insert((self.generation, page), key);
            }
            if self.preview.record_tx.send(job).is_err() {
                self.preview.pending.remove(&key);
            }
        }

        if let Some(recording) =
            self.preview
                .fallback_recording(page, ui.ctx().pixels_per_point(), key)
        {
            let _ = gpu.append_recording(ui, &recording, page_rect.min, scale);
            return true;
        }
        false
    }
}
/// Human-readable label for a whole-document GPU fallback reason.
#[cfg(feature = "gpu-canvas")]
fn render_error_label(err: &typst_gpu_canvas::RenderError) -> String {
    match err {
        typst_gpu_canvas::RenderError::UnsupportedCanvasContent("tiling") => {
            t("vector.tiling").into_owned()
        }
        typst_gpu_canvas::RenderError::UnsupportedCanvasContent("pdf image") => {
            t("vector.pdf_image").into_owned()
        }
        typst_gpu_canvas::RenderError::UnsupportedCanvasContent(kind) => (*kind).into(),
        other => other.to_string(),
    }
}

/// Raster-only preview backend: the GPU canvas is not compiled in, so every
/// method reports "no GPU path". Mirrors the `gpu-canvas` API so `preview.rs`
/// compiles unchanged and the raster path is always exercised.
#[cfg(not(feature = "gpu-canvas"))]
pub(super) struct VectorPreview {
    _private: (),
}

#[cfg(not(feature = "gpu-canvas"))]
impl VectorPreview {
    pub(super) fn new(_gpu: Option<GpuCanvasHandle>) -> Self {
        Self { _private: () }
    }

    pub(super) fn set_image_max_side(&mut self, _image_max_side: u32) {}

    pub(super) fn gpu_failed(&self, _generation: u64) -> bool {
        false
    }

    pub(super) fn fallback_reason(&self, _generation: u64) -> Option<&str> {
        None
    }

    pub(super) fn take_fallback_notice(&mut self, _generation: u64) -> Option<String> {
        None
    }

    pub(super) fn painter<'a>(&'a mut self, _doc: &'a DocResources) -> VectorPainter<'a> {
        VectorPainter { _preview: self }
    }
}

/// Paints preview pages through the GPU canvas, which is absent without the
/// `gpu-canvas` feature, so every page is reported unpaintable.
#[cfg(not(feature = "gpu-canvas"))]
pub(super) struct VectorPainter<'a> {
    _preview: &'a mut VectorPreview,
}

#[cfg(not(feature = "gpu-canvas"))]
impl typ_edit_preview::VectorPagePainter for VectorPainter<'_> {
    fn paint_page(
        &mut self,
        _ui: &mut egui::Ui,
        _page: usize,
        _page_rect: egui::Rect,
        _scale: f32,
    ) -> bool {
        false
    }
}

#[cfg(all(test, feature = "gpu-canvas"))]
mod tests {
    use super::*;

    fn failed(generation: u64, reason: &str) -> VectorPreview {
        let mut preview = VectorPreview::new(None);
        preview.failed_generation = Some(generation);
        preview.failed_reason = Some(reason.to_owned());
        preview
    }

    #[test]
    fn render_error_label_maps_known_kinds() {
        assert_eq!(
            render_error_label(&typst_gpu_canvas::RenderError::UnsupportedCanvasContent(
                "tiling"
            )),
            "平铺图案"
        );
        assert_eq!(
            render_error_label(&typst_gpu_canvas::RenderError::UnsupportedCanvasContent(
                "pdf image"
            )),
            "PDF 图片"
        );
        assert_eq!(
            render_error_label(&typst_gpu_canvas::RenderError::UnsupportedCanvasContent(
                "svg pattern"
            )),
            "svg pattern"
        );
        assert_eq!(
            render_error_label(&typst_gpu_canvas::RenderError::NonFiniteScale),
            "scale must be finite"
        );
    }

    #[test]
    fn fallback_reason_is_generation_scoped() {
        let preview = failed(7, "平铺图案");
        assert_eq!(preview.fallback_reason(7), Some("平铺图案"));
        assert_eq!(preview.fallback_reason(8), None);
    }

    #[test]
    fn fallback_notice_fires_once_per_generation() {
        let mut preview = failed(7, "平铺图案");
        assert_eq!(preview.take_fallback_notice(7), Some("平铺图案".into()));
        assert_eq!(preview.take_fallback_notice(7), None);
    }

    #[test]
    fn fallback_notice_ignores_unfailed_generations() {
        let mut preview = failed(7, "PDF 图片");
        assert_eq!(preview.take_fallback_notice(8), None);
        assert_eq!(preview.take_fallback_notice(7), Some("PDF 图片".into()));
    }

    #[test]
    fn a_later_failed_generation_notifies_again() {
        let mut preview = failed(7, "a");
        assert!(preview.take_fallback_notice(7).is_some());
        preview.failed_generation = Some(8);
        preview.failed_reason = Some("b".into());
        assert_eq!(preview.take_fallback_notice(8), Some("b".into()));
    }

    fn empty_frame() -> typst::layout::Frame {
        typst::layout::Frame::soft(typst::layout::Size::new(
            typst::layout::Abs::pt(10.0),
            typst::layout::Abs::pt(10.0),
        ))
    }

    fn empty_recording(ctx: &egui::Context) -> Arc<PageRecording> {
        let bvh = FrameBvh::new(empty_frame());
        Arc::new(
            typst_gpu_canvas::record_page(&bvh, egui::pos2(0.0, 0.0), 1.0, 1.0, ctx, 4096)
                .expect("empty page records"),
        )
    }

    struct EmptyFrames;

    impl PageFrameSource for EmptyFrames {
        fn frame(&self, page: usize) -> Option<typst::layout::Frame> {
            (page == 0).then(empty_frame)
        }
    }

    #[test]
    fn fallback_prefers_the_current_generation_then_the_nearest_bucket() {
        let ctx = egui::Context::default();
        let mut preview = VectorPreview::new(None);
        let recording = empty_recording(&ctx);
        preview.store_recording(RecordKey::new(1, 0, 1.0, 1.0), Arc::clone(&recording));
        preview.store_recording(
            RecordKey::new(2, 0, scale_bucket(1.19), 1.0),
            Arc::clone(&recording),
        );
        let wanted = RecordKey::new(2, 0, 2.0, 1.0);
        assert_eq!(
            preview.fallback_key(0, 1.0, wanted),
            Some(RecordKey::new(2, 0, scale_bucket(1.19), 1.0)),
            "the current generation and nearest bucket win"
        );
    }

    #[test]
    fn fallback_never_uses_a_generation_older_than_the_previous_one() {
        let ctx = egui::Context::default();
        let mut preview = VectorPreview::new(None);
        let recording = empty_recording(&ctx);
        preview.previous_generation = Some(2);
        preview.generation = 3;
        preview.store_recording(RecordKey::new(1, 0, 1.0, 1.0), Arc::clone(&recording));
        preview.store_recording(
            RecordKey::new(2, 0, scale_bucket(1.19), 1.0),
            Arc::clone(&recording),
        );
        let wanted = RecordKey::new(3, 0, 1.0, 1.0);
        assert_eq!(
            preview.fallback_key(0, 1.0, wanted),
            Some(RecordKey::new(2, 0, scale_bucket(1.19), 1.0)),
            "only the immediately previous generation may be shown"
        );
    }

    #[test]
    fn fallback_is_absent_when_only_older_generations_remain() {
        let ctx = egui::Context::default();
        let mut preview = VectorPreview::new(None);
        preview.previous_generation = Some(2);
        preview.generation = 3;
        preview.store_recording(RecordKey::new(1, 0, 1.0, 1.0), empty_recording(&ctx));
        assert_eq!(
            preview.fallback_key(0, 1.0, RecordKey::new(3, 0, 1.0, 1.0)),
            None
        );
    }

    #[test]
    fn replies_from_an_older_generation_are_rejected() {
        let mut preview = VectorPreview::new(None);
        preview.previous_generation = Some(2);
        preview.generation = 3;
        assert!(preview.accepts_reply(RecordKey::new(3, 0, 1.0, 1.0)));
        assert!(preview.accepts_reply(RecordKey::new(2, 0, 1.0, 1.0)));
        assert!(!preview.accepts_reply(RecordKey::new(1, 0, 1.0, 1.0)));
    }

    #[test]
    fn changing_image_max_side_invalidates_pending_replies() {
        let mut preview = VectorPreview::new(None);
        let old = RecordKey::new(u64::MAX, 0, 1.0, 1.0);
        preview.wanted.insert((u64::MAX, 0), old);
        preview.pending.insert(old);

        preview.set_image_max_side(8192);

        assert!(preview.wanted.is_empty());
        assert!(preview.pending.is_empty());
        assert!(!preview.accepts_reply(old));
        assert!(preview.accepts_reply(RecordKey::with_image_max_side(u64::MAX, 0, 1.0, 1.0, 8192)));
    }

    #[test]
    fn switching_generation_cancels_jobs_older_than_previous() {
        let mut preview = VectorPreview::new(None);
        preview.previous_generation = Some(2);
        preview.generation = 3;
        if let Ok(mut latest) = preview.latest.lock() {
            for generation in [1, 2, 3] {
                latest.insert((generation, 0), RecordKey::new(generation, 0, 1.0, 1.0));
            }
        }
        preview.retain_previous_jobs();
        let latest = preview.latest.lock().expect("latest map");
        assert!(!latest.contains_key(&(1, 0)));
        assert!(latest.contains_key(&(2, 0)));
        assert!(!latest.contains_key(&(3, 0)));
    }

    #[test]
    fn a_new_generation_keeps_the_old_recording_as_a_fallback() {
        let ctx = egui::Context::default();
        let mut preview = VectorPreview::new(None);
        let old = RecordKey::new(1, 0, 1.0, 1.0);
        let new = RecordKey::new(2, 0, 1.0, 1.0);
        preview.store_recording(old, empty_recording(&ctx));
        preview.store_recording(new, empty_recording(&ctx));
        assert!(preview.recording(old).is_some());
        assert!(preview.recording(new).is_some());
    }

    #[test]
    fn the_recording_worker_builds_the_bvh_and_returns_a_recording() {
        let (tx, rx, latest) = recording_worker();
        let key = RecordKey::new(3, 0, 1.0, 1.0);
        latest.lock().expect("latest map").insert((3, 0), key);
        tx.send(RecordJob {
            key,
            page: 0,
            source: Arc::new(EmptyFrames),
            bvh: None,
            pos: (0.0, 0.0),
            scale: key.bucket(),
            ppp: 1.0,
            ctx: egui::Context::default(),
            max_texture_side: 4096,
        })
        .expect("send record job");
        let reply = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("recording reply");
        assert_eq!(reply.key, key);
        assert!(reply.error.is_none());
        assert!(reply.bvh.is_some());
        assert!(reply.recording.is_some());
    }
}
