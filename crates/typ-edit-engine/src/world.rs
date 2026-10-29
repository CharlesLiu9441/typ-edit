//! The typst [`World`](typst::World) implementation: fonts, files and the in-memory
//! editor overlay.
//!
//! One [`WorldState`] is built per compilation by the compile worker. Files are
//! addressed relative to `root`; the overlay (unsaved editor buffers) always wins over
//! the on-disk contents.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::sync::lock;

use typst::Library;
use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst_kit::packages::SystemPackages;

/// Everything the typst compiler needs for one compilation.
pub struct WorldState {
    /// Project root; virtual paths resolve relative to it.
    root: PathBuf,
    /// Absolute path of the compilation entry file.
    main_path: PathBuf,
    /// Interned id of the entry file.
    main: FileId,
    /// In-memory overrides (absolute path → contents) for unsaved editor buffers.
    overlay: HashMap<PathBuf, String>,
    /// System + embedded fonts, shared across compiles by the [`FontCatalog`].
    ///
    /// [`FontCatalog`]: crate::fonts::FontCatalog
    fonts: Arc<typst_kit::fonts::FontStore>,
    /// Package loader (system data/cache + Typst Universe), shared across compiles.
    packages: Arc<SystemPackages>,
    /// The standard library, built once and shared by every world.
    library: &'static LazyHash<Library>,
    /// Mapping from interned file ids back to filesystem paths.
    paths: Mutex<HashMap<FileId, PathBuf>>,
    /// Loaded sources, so the compiler's repeated queries stay cheap.
    sources: Mutex<HashMap<FileId, Source>>,
    /// Project-relative paths whose on-disk read returned `NotFound` during this
    /// compilation. The app turns these into server downloads and recompiles.
    missing: Mutex<BTreeSet<String>>,
}

impl WorldState {
    /// Builds a world for `root`/`main` with the given in-memory overlay.
    pub fn new(
        root: PathBuf,
        main: PathBuf,
        overlay: Vec<(PathBuf, String)>,
        fonts: &crate::fonts::FontCatalog,
        packages: Arc<SystemPackages>,
    ) -> Self {
        let main_id = file_id_for_path(&root, &main).unwrap_or_else(fallback_main_id);

        let mut paths = HashMap::new();
        paths.insert(main_id, main.clone());

        let overlay: HashMap<PathBuf, String> = overlay.into_iter().collect();
        let mut overlay_ids = Vec::new();
        for path in overlay.keys() {
            if let Some(id) = file_id_for_path(&root, path) {
                paths.entry(id).or_insert_with(|| path.clone());
                overlay_ids.push(id);
            }
        }

        let world = Self {
            root,
            main_path: main,
            main: main_id,
            overlay,
            fonts: fonts.build_typst_font_store(),
            packages,
            library: crate::stdlib::lazy_library(),
            paths: Mutex::new(paths),
            sources: Mutex::new(HashMap::new()),
            missing: Mutex::new(BTreeSet::new()),
        };

        // Pre-load the entry file (and every overlay file) so the first compiler
        // query hits the cache instead of the file system.
        let _ = <Self as typst::World>::source(&world, main_id);
        for id in overlay_ids {
            let _ = <Self as typst::World>::source(&world, id);
        }
        world
    }

    /// Path of the compilation entry file.
    pub fn main_path(&self) -> &Path {
        &self.main_path
    }

    /// Maps a typst [`FileId`] back to a filesystem path (for diagnostics / jump).
    pub fn path_of(&self, id: FileId) -> Option<PathBuf> {
        self.resolve_path(id).ok()
    }

    /// Resolves the real path of a file, obtaining packages on demand.
    ///
    /// Project files realize against `root`; package files are obtained from the
    /// system package loader (data dir → cache dir → registry).
    fn resolve_path(&self, id: FileId) -> FileResult<PathBuf> {
        if id == self.main {
            return Ok(self.main_path.clone());
        }
        if let Some(path) = lock(&self.paths).get(&id) {
            return Ok(path.clone());
        }

        let path = match id.root() {
            VirtualRoot::Project => id
                .vpath()
                .realize(&self.root)
                .map_err(|_| FileError::NotFound(PathBuf::from(id.vpath().get_with_slash())))?,
            VirtualRoot::Package(spec) => self
                .packages
                .obtain(spec)
                .map_err(FileError::Package)?
                .resolve(id.vpath())?,
        };

        lock(&self.paths).entry(id).or_insert_with(|| path.clone());
        Ok(path)
    }

    /// Loads the source of a file as a [`Source`] (for jump_from_cursor).
    pub fn source_of(&self, path: &Path) -> Option<Source> {
        let id = if path == self.main_path {
            self.main
        } else {
            file_id_for_path(&self.root, path)?
        };
        if id != self.main {
            lock(&self.paths)
                .entry(id)
                .or_insert_with(|| path.to_path_buf());
        }
        <Self as typst::World>::source(self, id).ok()
    }

    /// Returns the overlay contents of `path` if the buffer is unsaved.
    fn overlay_text(&self, path: &Path) -> Option<&String> {
        self.overlay.get(path)
    }

    fn note_missing(&self, id: FileId, error: &std::io::Error) {
        if error.kind() == std::io::ErrorKind::NotFound && matches!(id.root(), VirtualRoot::Project)
        {
            lock(&self.missing).insert(id.vpath().get_without_slash().to_owned());
        }
    }

    /// Project-relative files whose reads returned `NotFound` during this
    /// compilation. Package files are intentionally excluded.
    pub fn missing_paths(&self) -> Vec<String> {
        lock(&self.missing).iter().cloned().collect()
    }
}

impl typst::World for WorldState {
    fn library(&self) -> &LazyHash<Library> {
        self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.fonts.book()
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if let Some(source) = lock(&self.sources).get(&id) {
            return Ok(source.clone());
        }

        let path = self.resolve_path(id)?;
        let text = match self.overlay_text(&path) {
            Some(text) => text.clone(),
            None => match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(err) => {
                    self.note_missing(id, &err);
                    return Err(FileError::from_io(err, &path));
                }
            },
        };

        let source = Source::new(id, text);
        lock(&self.sources).insert(id, source.clone());
        Ok(source)
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        let path = self.resolve_path(id)?;

        if let Some(text) = self.overlay_text(&path) {
            return Ok(Bytes::from_string(text.clone()));
        }
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Bytes::new(bytes)),
            Err(err) => {
                self.note_missing(id, &err);
                Err(FileError::from_io(err, &path))
            }
        }
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.font(index)
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        // No live clock: reproducible builds and no `datetime.today()` surprises.
        None
    }
}

impl typst_ide::IdeWorld for WorldState {
    fn upcast(&self) -> &dyn typst::World {
        self
    }
}

/// Interns the id for a real path inside `root`.
fn file_id_for_path(root: &Path, path: &Path) -> Option<FileId> {
    let vpath = vpath_for_path(root, path)?;
    Some(RootedPath::new(VirtualRoot::Project, vpath).intern())
}

/// Builds the virtual path of `path` below `root`.
///
/// `VirtualPath::virtualize` strips the root prefix and normalizes the result; if the
/// path lies outside the root (e.g. a preview target picked elsewhere), it is addressed
/// by its file name alone so the world stays usable.
fn vpath_for_path(root: &Path, path: &Path) -> Option<VirtualPath> {
    if let Ok(vpath) = VirtualPath::virtualize(root, path) {
        return Some(vpath);
    }
    let name = path.file_name()?.to_str()?;
    VirtualPath::new(name).ok()
}

/// Id used when the main file cannot even be named (empty path).
fn fallback_main_id() -> FileId {
    RootedPath::new(
        VirtualRoot::Project,
        VirtualPath::new("main.typ").expect("`main.typ` is a valid virtual path"),
    )
    .intern()
}

#[cfg(test)]
mod tests {
    use super::*;
    use typst::World as _;
    use typst::diag::PackageError;
    use typst::syntax::package::PackageSpec;
    use typst_kit::packages::{FsPackages, UniversePackages};

    /// A downloader that never leaves the machine.
    struct OfflineDownloader;

    impl typst_kit::downloader::Downloader for OfflineDownloader {
        fn stream(
            &self,
            _key: &dyn std::any::Any,
            _url: &str,
        ) -> std::io::Result<(Option<usize>, Box<dyn std::io::Read>)> {
            Err(std::io::ErrorKind::NotFound.into())
        }
    }

    /// Package loader with no data/cache dirs and an offline downloader.
    fn offline_packages() -> Arc<SystemPackages> {
        Arc::new(SystemPackages::from_parts(
            None,
            None,
            UniversePackages::new(OfflineDownloader),
        ))
    }

    /// The workers share `WorldState` across threads; this must stay true.
    #[test]
    fn world_state_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<WorldState>();
        assert_send_sync::<Arc<WorldState>>();
    }

    #[test]
    fn main_and_overlay_are_preferred() {
        let catalog = crate::fonts::FontCatalog::new();
        let root = std::env::temp_dir();
        let main = root.join("typ-edit-world-test.typ");
        let overlay_path = root.join("typ-edit-world-overlay.typ");

        let world = WorldState::new(
            root.clone(),
            main.clone(),
            vec![(overlay_path.clone(), "overlay content".to_owned())],
            &catalog,
            offline_packages(),
        );

        assert_eq!(world.main_path(), main.as_path());
        assert_eq!(world.path_of(world.main), Some(main.clone()));

        // The overlay file is addressable and its content is served from memory.
        let source = world.source_of(&overlay_path).expect("overlay source");
        assert_eq!(source.text(), "overlay content");

        // ... and maps back to the path we registered.
        assert_eq!(world.path_of(source.id()), Some(overlay_path));
        assert!(world.source_of(&root.join("does-not-exist.typ")).is_none());
    }

    #[test]
    fn package_source_resolves_from_data_dir() {
        let catalog = crate::fonts::FontCatalog::new();
        let root = std::env::temp_dir();
        let data = root.join(format!("typ-edit-package-data-{}", std::process::id()));
        let pkg_dir = data.join("preview").join("cetz").join("0.3.1");
        std::fs::create_dir_all(&pkg_dir).expect("create package dir");
        std::fs::write(pkg_dir.join("lib.typ"), "package content").expect("write package file");

        let packages = Arc::new(SystemPackages::from_parts(
            Some(FsPackages::new(data.clone())),
            None,
            UniversePackages::new(OfflineDownloader),
        ));
        let world = WorldState::new(
            root.clone(),
            root.join("typ-edit-package-main.typ"),
            Vec::new(),
            &catalog,
            packages,
        );

        let spec: PackageSpec = "@preview/cetz:0.3.1".parse().expect("valid spec");
        let id = RootedPath::new(
            VirtualRoot::Package(spec),
            VirtualPath::new("lib.typ").expect("valid vpath"),
        )
        .intern();

        let source = world.source(id).expect("package source");
        assert_eq!(source.text(), "package content");
        assert_eq!(world.path_of(id), Some(pkg_dir.join("lib.typ")));

        std::fs::remove_dir_all(&data).ok();
    }

    #[test]
    fn missing_package_surfaces_package_error() {
        let catalog = crate::fonts::FontCatalog::new();
        let root = std::env::temp_dir();
        let world = WorldState::new(
            root.clone(),
            root.join("typ-edit-missing-package-main.typ"),
            Vec::new(),
            &catalog,
            offline_packages(),
        );

        let spec: PackageSpec = "@preview/does-not-exist:0.0.1".parse().expect("valid spec");
        let id = RootedPath::new(
            VirtualRoot::Package(spec),
            VirtualPath::new("lib.typ").expect("valid vpath"),
        )
        .intern();

        match world.source(id) {
            Err(FileError::Package(PackageError::NotFound(_))) => {}
            other => panic!("expected package not found, got {other:?}"),
        }
        assert!(world.path_of(id).is_none());
    }
}
