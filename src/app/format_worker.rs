//! Off-UI document formatting.
//!
//! Typstyle parses and pretty-prints the whole source, so running it on the UI
//! thread stalls large documents. The request carries an O(1) [`TextSnapshot`];
//! the worker materializes and formats it, then the frame pump applies the
//! result only when the tab still holds the same buffer revision.

use std::sync::mpsc::{self, Receiver, Sender};

use typ_edit_model::TextSnapshot;

pub(super) enum FormatOutcome {
    Empty,
    Skipped,
    Formatted(String),
}

/// Why a formatter request was sent. A save reply must write after applying
/// the formatted text; a manual reply only updates the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FormatPurpose {
    Manual,
    Save,
}

pub(super) struct FormatReply {
    pub(super) tab_id: u64,
    pub(super) source_rev: u64,
    pub(super) purpose: FormatPurpose,
    pub(super) outcome: FormatOutcome,
}

struct FormatRequest {
    tab_id: u64,
    source_rev: u64,
    purpose: FormatPurpose,
    snapshot: TextSnapshot,
    ctx: egui::Context,
}

pub(super) struct FormatWorker {
    tx: Sender<FormatRequest>,
    rx: Receiver<FormatReply>,
    busy: bool,
}

impl Default for FormatWorker {
    fn default() -> Self {
        Self::spawn()
    }
}

impl FormatWorker {
    fn spawn() -> Self {
        let (tx, requests) = mpsc::channel::<FormatRequest>();
        let (replies, rx) = mpsc::channel::<FormatReply>();
        let spawned = std::thread::Builder::new()
            .name("typ-edit-format".to_owned())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    let text = request.snapshot.materialize();
                    let outcome = if text.trim().is_empty() {
                        FormatOutcome::Empty
                    } else {
                        match crate::format::format_source(&text) {
                            Some(formatted) => FormatOutcome::Formatted(formatted),
                            None => FormatOutcome::Skipped,
                        }
                    };
                    if replies
                        .send(FormatReply {
                            tab_id: request.tab_id,
                            source_rev: request.source_rev,
                            purpose: request.purpose,
                            outcome,
                        })
                        .is_err()
                    {
                        break;
                    }
                    request.ctx.request_repaint();
                }
            });
        if let Err(err) = spawned {
            log::error!("cannot start format worker: {err}");
        }
        Self {
            tx,
            rx,
            busy: false,
        }
    }

    pub(super) fn is_busy(&self) -> bool {
        self.busy
    }

    pub(super) fn request(
        &mut self,
        tab_id: u64,
        snapshot: TextSnapshot,
        ctx: egui::Context,
        purpose: FormatPurpose,
    ) -> bool {
        if self.busy {
            return false;
        }
        let source_rev = snapshot.rev();
        let request = FormatRequest {
            tab_id,
            source_rev,
            purpose,
            snapshot,
            ctx,
        };
        if self.tx.send(request).is_err() {
            return false;
        }
        self.busy = true;
        true
    }

    pub(super) fn poll(&mut self) -> Option<FormatReply> {
        let reply = self.rx.try_recv().ok()?;
        self.busy = false;
        Some(reply)
    }

    #[cfg(test)]
    pub(super) fn wait(&mut self) -> Option<FormatReply> {
        let reply = self
            .rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .ok()?;
        self.busy = false;
        Some(reply)
    }
}
