//! A thread-safe handle to an immutable snapshot of editor text.
//!
//! The concrete source (a rope clone, an `Arc<str>`, …) is supplied by the crate
//! that owns the text. This module only defines the cross-thread contract, so a
//! worker thread can hold a cheap handle and materialize the text later, on its
//! own thread, without the producer paying for a copy up front.

use std::fmt;
use std::sync::Arc;

/// An immutable snapshot of editor text.
///
/// Implementors expose a stable [`rev`](TextSource::rev) and can produce the full
/// text on demand via [`materialize`](TextSource::materialize). The `Send + Sync`
/// bound lets a snapshot cross a worker-thread channel; materialization is
/// deliberately explicit (no `Display`) because it may be expensive and belongs
/// on the consuming thread.
pub trait TextSource: Send + Sync + 'static {
    /// The exact text revision this snapshot captured. Revisions are strictly
    /// monotonic across a document's lifetime; a snapshot's revision never changes.
    fn rev(&self) -> u64;

    /// The full text of this snapshot. Callers are worker threads; the explicit
    /// name (rather than `Display`) keeps the cost visible at every call site.
    fn materialize(&self) -> String;
}

/// A cheap-to-clone, thread-safe handle to a [`TextSource`].
///
/// Cloning shares the same underlying source; the text is only produced when
/// [`materialize`](TextSnapshot::materialize) is called, so passing a handle
/// across a channel never copies the document.
#[derive(Clone)]
pub struct TextSnapshot(Arc<dyn TextSource>);

impl TextSnapshot {
    /// Wraps `source` in a shared handle.
    pub fn new(source: impl TextSource) -> Self {
        Self(Arc::new(source))
    }

    /// Builds a snapshot from already-owned text.
    ///
    /// For tests and callers that have a `String` rather than a rope: the text
    /// is moved into a dependency-free [`OwnedText`] source. `rev` is the
    /// revision the caller assigns to `text`.
    pub fn from_owned(text: impl Into<String>, rev: u64) -> Self {
        Self::new(OwnedText {
            text: text.into(),
            rev,
        })
    }

    /// The revision captured by this snapshot.
    pub fn rev(&self) -> u64 {
        self.0.rev()
    }

    /// Materializes the full text. May be expensive; call on the consuming thread.
    pub fn materialize(&self) -> String {
        self.0.materialize()
    }
}

/// A [`TextSource`] over an owned `String`, used by [`TextSnapshot::from_owned`].
struct OwnedText {
    text: String,
    rev: u64,
}

impl TextSource for OwnedText {
    fn rev(&self) -> u64 {
        self.rev
    }

    fn materialize(&self) -> String {
        self.text.clone()
    }
}

impl fmt::Debug for TextSnapshot {
    /// Shows only the revision — never materializes the text, so logging a handle
    /// cannot trigger a costly copy.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("TextSnapshot").field(&self.rev()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Minimal [`TextSource`] that records how many times it materializes.
    struct FakeSource {
        rev: u64,
        text: String,
        materializations: Arc<AtomicUsize>,
    }

    impl FakeSource {
        /// Returns the source plus a shared counter the test asserts against.
        fn new(rev: u64, text: &str) -> (Self, Arc<AtomicUsize>) {
            let materializations = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    rev,
                    text: text.to_owned(),
                    materializations: Arc::clone(&materializations),
                },
                materializations,
            )
        }
    }

    impl TextSource for FakeSource {
        fn rev(&self) -> u64 {
            self.rev
        }

        fn materialize(&self) -> String {
            self.materializations.fetch_add(1, Ordering::SeqCst);
            self.text.clone()
        }
    }

    #[test]
    fn round_trips_rev_and_text() {
        let (source, _) = FakeSource::new(7, "hello");
        let snapshot = TextSnapshot::new(source);
        assert_eq!(snapshot.rev(), 7);
        assert_eq!(snapshot.materialize(), "hello");
    }

    #[test]
    fn from_owned_round_trips_rev_and_text() {
        let snapshot = TextSnapshot::from_owned("owned text", 9);
        assert_eq!(snapshot.rev(), 9);
        assert_eq!(snapshot.materialize(), "owned text");

        let clone = snapshot.clone();
        assert!(
            Arc::ptr_eq(&snapshot.0, &clone.0),
            "a clone shares the one owned source"
        );
        assert_eq!(clone.materialize(), "owned text");
    }

    #[test]
    fn clones_share_one_underlying_source() {
        let (source, materializations) = FakeSource::new(1, "shared");
        let snapshot = TextSnapshot::new(source);
        let clone = snapshot.clone();

        assert!(
            Arc::ptr_eq(&snapshot.0, &clone.0),
            "clones share the same Arc"
        );
        // Both calls route to the one shared source.
        assert_eq!(snapshot.materialize(), "shared");
        assert_eq!(clone.materialize(), "shared");
        assert_eq!(materializations.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn is_send_sync_clone() {
        fn assert_traits<T: Send + Sync + Clone>() {}
        assert_traits::<TextSnapshot>();
    }

    #[test]
    fn debug_shows_rev_without_materializing() {
        let (source, materializations) = FakeSource::new(42, "secret");
        let snapshot = TextSnapshot::new(source);
        let rendered = format!("{snapshot:?}");
        assert!(
            rendered.contains("42"),
            "debug shows the revision: {rendered}"
        );
        assert_eq!(
            materializations.load(Ordering::SeqCst),
            0,
            "Debug must not materialize the text"
        );
    }
}
