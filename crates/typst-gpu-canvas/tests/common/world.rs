//! A minimal [`typst::World`] for compiling fixture documents in tests.
//!
//! The suite renders *real* Typst documents, so it needs a compiler world.
//! This is the smallest world that can compile a single in-memory `main.typ`:
//!
//! * [`library`](World::library) - the default standard library.
//! * [`book`](World::book) / [`font`](World::font) - every font bundled in
//!   `typst-assets`, shared once per test process through a `LazyLock`. The
//!   book is built from each asset via `Font::iter`, which enumerates a font
//!   collection with `Font::new(data, index)`.
//! * [`main`](World::main) - a virtual `main.typ` rooted at the project.
//! * [`source`](World::source) / [`file`](World::file) - the in-memory source,
//!   and `NotFound` for anything else (no packages, no assets).
//! * [`today`](World::today) - a fixed epoch datetime so output is
//!   deterministic; the fixtures do not read the clock.
//!
//! The 0.15.1 `World` trait has no `rng` method (the `random` function does not
//! consult the world in this version), so there is nothing to stub for it.

use std::sync::LazyLock;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World as TypstWorld};

/// Every bundled font and its book, built once per test process.
static FONTS: LazyLock<(LazyHash<FontBook>, Vec<Font>)> = LazyLock::new(|| {
    let fonts: Vec<Font> = typst_assets::fonts()
        .flat_map(|data| Font::iter(Bytes::new(data)))
        .collect();
    let book = FontBook::from_fonts(&fonts);
    (LazyHash::new(book), fonts)
});

/// A minimal single-file Typst world.
pub struct World {
    library: LazyHash<Library>,
    main: FileId,
    source: Source,
}

impl World {
    /// Creates a world whose `main.typ` is `text`.
    pub fn new(text: &str) -> Self {
        let main = RootedPath::new(
            VirtualRoot::Project,
            VirtualPath::new("main.typ").expect("static virtual path is valid"),
        )
        .intern();
        let source = Source::new(main, text.to_owned());
        Self {
            library: LazyHash::new(Library::builder().build()),
            main,
            source,
        }
    }
}

impl TypstWorld for World {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &FONTS.0
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main {
            Ok(self.source.clone())
        } else {
            Err(FileError::NotFound(id.vpath().get_without_slash().into()))
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.main {
            Ok(Bytes::from_string(self.source.clone()))
        } else {
            Err(FileError::NotFound(id.vpath().get_without_slash().into()))
        }
    }

    fn font(&self, index: usize) -> Option<Font> {
        FONTS.1.get(index).cloned()
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        // A fixed, timezone-independent timestamp: the fixtures never use the
        // clock, but a deterministic value keeps any accidental use stable.
        Datetime::from_ymd_hms(1970, 1, 1, 12, 0, 0)
    }
}

/// Compiles `source` as `main.typ` and returns the paged document.
///
/// Panics with the compiler diagnostics if the fixture does not compile - a
/// broken fixture is a test bug, not a rendering difference.
pub fn compile(source: &str) -> typst_layout::PagedDocument {
    let world = World::new(source);
    let warned = typst::compile::<typst_layout::PagedDocument>(&world);
    match warned.output {
        Ok(document) => document,
        Err(errors) => panic!("fixture failed to compile: {errors:?}"),
    }
}
