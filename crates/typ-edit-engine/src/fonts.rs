//! System font catalog: enumeration for the settings UI and the typst font store
//! used by the compile worker.
//!
//! The catalog owns one `fontdb::Database` (system scan happens in [`FontCatalog::new`]).
//! Resolved font bytes and the typst [`FontStore`] are cached so that neither compilation
//! nor font resolution pays for a system scan more than once.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use crate::sync::lock;
use fontdb::{Database, FaceInfo, Source, Style, Weight};
use typst::text::FontInfo;
use typst_kit::fonts::{self, FontPath, FontStore};

/// Upper bound on cached [`resolve`](FontCatalog::resolve) results (including
/// "not installed" misses); past it the least-recently-used key is evicted.
const RESOLVED_CACHE_CAP: usize = 10_000;

/// A font face resolved to bytes (for egui / custom loading).
#[derive(Clone)]
pub struct ResolvedFont {
    pub family: String,
    pub data: Arc<Vec<u8>>,
    pub index: u32,
}

/// Approximate LRU over resolved fonts: the lookup map plus its keys in
/// recency order, oldest first. `None` values (not-installed families) count
/// toward the cap too.
#[derive(Default)]
struct ResolvedCache {
    entries: HashMap<String, Option<ResolvedFont>>,
    order: VecDeque<String>,
}

impl ResolvedCache {
    /// Moves `key` to the most-recent end of the recency order.
    fn touch(&mut self, key: &str) {
        if let Some(pos) = self.order.iter().position(|candidate| candidate == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.to_owned());
    }

    /// Inserts `key`; evicts least-recently-used keys until the cap holds.
    fn insert(&mut self, key: String, value: Option<ResolvedFont>) {
        if self.entries.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > RESOLVED_CACHE_CAP {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

/// Catalog of installed system fonts. Built once at startup.
pub struct FontCatalog {
    /// System font database (also provides the raw bytes on demand).
    db: Database,
    /// Sorted, deduplicated family names.
    families: Vec<String>,
    /// Cache of [`resolve`](Self::resolve) results, keyed by the requested family
    /// name (lowercased). `None` means "not installed".
    resolved: Mutex<ResolvedCache>,
    /// Lazily built typst font store, shared with every compile.
    font_store: OnceLock<Arc<FontStore>>,
}

impl FontCatalog {
    /// Scans system fonts (fontdb). May take a moment on first call.
    pub fn new() -> Self {
        let mut db = Database::new();
        db.load_system_fonts();
        // Mirror `typst_kit::fonts::system`, which previously supplied the
        // typst store and also picked up Adobe's user fonts.
        load_adobe_fonts(&mut db);
        Self::from_db(db)
    }

    /// An empty catalog: no system families, only the embedded typst fonts in
    /// the font store. The UI uses it until the background scan lands.
    pub fn empty() -> Self {
        Self::from_db(Database::new())
    }

    fn from_db(db: Database) -> Self {
        let families = collect_families(&db);
        Self {
            db,
            families,
            resolved: Mutex::new(ResolvedCache::default()),
            font_store: OnceLock::new(),
        }
    }

    /// Sorted, deduplicated family names for the settings pickers.
    pub fn families(&self) -> &[String] {
        &self.families
    }

    pub fn has_family(&self, name: &str) -> bool {
        self.find_family(name).is_some()
    }

    /// Resolves a family to bytes; tries the family, then common fallbacks.
    pub fn resolve(&self, family: &str) -> Option<ResolvedFont> {
        let key = family.to_lowercase();
        {
            let mut cache = lock(&self.resolved);
            if let Some(cached) = cache.entries.get(&key).cloned() {
                cache.touch(&key);
                return cached;
            }
        }

        let resolved = self.load_family(&key).map(|font| ResolvedFont {
            family: font.family,
            data: font.data,
            index: font.index,
        });

        lock(&self.resolved).insert(key, resolved.clone());
        resolved
    }

    /// First installed family the OS declares as monospaced, if any.
    ///
    /// A generic capability query, not a preference: the caller decides which
    /// monospaced family (if any) to prefer.
    pub fn monospaced_family(&self) -> Option<String> {
        for face in self.db.faces() {
            if face.monospaced
                && let Some((name, _)) = face.families.first()
            {
                return Some(name.clone());
            }
        }
        None
    }

    /// Builds the typst font store: all system fonts + typst embedded fonts.
    ///
    /// The store is built from the catalog's already-scanned `fontdb`, so it does
    /// not trigger a second OS font scan (the old `fonts::system()` path did).
    /// The first call caches the store; every later compile returns the same
    /// `Arc` for free. System fonts come first and the embedded fonts act as
    /// fallbacks, matching the typst CLI precedence.
    pub fn build_typst_font_store(&self) -> Arc<FontStore> {
        self.font_store
            .get_or_init(|| {
                let mut store = FontStore::new();
                let faces: Vec<(FontPath, FontInfo)> = self
                    .db
                    .faces()
                    .filter_map(|face| {
                        let path = match &face.source {
                            Source::File(path) | Source::SharedFile(path, _) => path.clone(),
                            Source::Binary(_) => return None,
                        };
                        let info = self.db.with_face_data(face.id, FontInfo::new).flatten()?;
                        Some((
                            FontPath {
                                path,
                                index: face.index,
                            },
                            info,
                        ))
                    })
                    .collect();
                store.extend(faces);
                store.extend(fonts::embedded());
                Arc::new(store)
            })
            .clone()
    }

    /// Case-insensitive exact lookup that returns the installed spelling.
    fn find_family(&self, name: &str) -> Option<String> {
        let key = name.to_lowercase();
        self.families
            .iter()
            .find(|family| family.to_lowercase() == key)
            .cloned()
    }

    /// Picks the best face of `key` (lowercased family name) and loads its bytes.
    fn load_family(&self, key: &str) -> Option<ResolvedFont> {
        let id = self.pick_face(key)?;
        let (data, index) = self
            .db
            .with_face_data(id, |data, index| (data.to_vec(), index))?;
        let family = self
            .db
            .face(id)
            .and_then(|face| face.families.first())
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| key.to_owned());
        Some(ResolvedFont {
            family,
            data: Arc::new(data),
            index,
        })
    }

    /// Chooses the face for a lowercased family name: exact matches win, and among
    /// them the one closest to normal weight/style.
    fn pick_face(&self, key: &str) -> Option<fontdb::ID> {
        let mut best: Option<(&FaceInfo, u8)> = None;
        let mut partial: Option<fontdb::ID> = None;

        for face in self.db.faces() {
            let names = || face.families.iter().map(|(name, _)| name.to_lowercase());
            if names().any(|name| name == key) {
                let score = face_score(face);
                if best.is_none_or(|(_, best_score)| score < best_score) {
                    best = Some((face, score));
                }
            } else if partial.is_none() && names().any(|name| name.contains(key)) {
                partial = Some(face.id);
            }
        }

        best.map(|(face, _)| face.id).or(partial)
    }
}

impl Default for FontCatalog {
    fn default() -> Self {
        Self::new()
    }
}

/// A font catalog that is scanned on a background thread.
///
/// The app installs egui's built-in fonts plus the bundled math faces
/// immediately, then swaps in the configured families once the scan lands.
/// Background workers call [`Fonts::wait`] before compiling, so the UI thread
/// never pays for the system-font scan.
pub struct Fonts {
    catalog: OnceLock<Arc<FontCatalog>>,
    ready: Mutex<bool>,
    ready_cv: Condvar,
    empty: Arc<FontCatalog>,
}

impl Fonts {
    /// Starts the system scan on a background thread and returns immediately.
    pub fn start() -> Arc<Self> {
        let fonts = Arc::new(Self::new());
        let worker = Arc::clone(&fonts);
        let spawned = std::thread::Builder::new()
            .name("typst-font-scan".to_owned())
            .spawn(move || {
                let scanned = std::panic::catch_unwind(FontCatalog::new).unwrap_or_else(|_| {
                    log::error!("system font scan panicked; using embedded fonts only");
                    FontCatalog::empty()
                });
                worker.install(scanned);
            });
        if let Err(error) = spawned {
            log::error!("cannot spawn the font scan thread: {error}");
            fonts.install(FontCatalog::empty());
        }
        fonts
    }

    /// A handle that is already loaded, for tests and headless callers.
    pub fn ready(catalog: FontCatalog) -> Arc<Self> {
        let fonts = Arc::new(Self::new());
        fonts.install(catalog);
        fonts
    }

    fn new() -> Self {
        Self {
            catalog: OnceLock::new(),
            ready: Mutex::new(false),
            ready_cv: Condvar::new(),
            empty: Arc::new(FontCatalog::empty()),
        }
    }

    fn install(&self, catalog: FontCatalog) {
        let _ = self.catalog.set(Arc::new(catalog));
        *lock(&self.ready) = true;
        self.ready_cv.notify_all();
    }

    /// The current catalog: the scanned one when ready, else an empty placeholder.
    pub fn current(&self) -> Arc<FontCatalog> {
        self.catalog
            .get()
            .cloned()
            .unwrap_or_else(|| Arc::clone(&self.empty))
    }

    /// Whether the background scan has landed.
    pub fn is_ready(&self) -> bool {
        *lock(&self.ready)
    }

    /// Blocks until the scan lands. Only background workers should call this.
    pub fn wait(&self) -> Arc<FontCatalog> {
        let mut ready = lock(&self.ready);
        while !*ready {
            ready = self
                .ready_cv
                .wait(ready)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        self.catalog
            .get()
            .cloned()
            .unwrap_or_else(|| Arc::clone(&self.empty))
    }
}

/// Loads Adobe Fonts from their OS-specific user directories, matching the
/// typst-kit system scanner that the store used before it switched to this
/// catalog's database.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn load_adobe_fonts(db: &mut Database) {
    let Some(base) = adobe_fonts_base() else {
        return;
    };
    let prefix = if cfg!(target_os = "macos") { "." } else { "" };
    for subdir in [
        format!("Adobe/CoreSync/plugins/livetype/{prefix}r"),
        format!("Adobe/{prefix}User Owned Fonts"),
    ] {
        let Ok(entries) = std::fs::read_dir(base.join(subdir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let is_file = entry
                .metadata()
                .map(|metadata| metadata.is_file())
                .unwrap_or(false);
            if is_file {
                let _ = db.load_font_file(entry.path());
            }
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn adobe_fonts_base() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn load_adobe_fonts(_db: &mut Database) {}

/// Collects sorted, deduplicated family names from the database.
fn collect_families(db: &Database) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut families = Vec::new();
    for face in db.faces() {
        for (name, _) in &face.families {
            if seen.insert(name.to_lowercase()) {
                families.push(name.clone());
            }
        }
    }
    families.sort_by_key(|name| name.to_lowercase());
    families
}

/// Lower is better: normal style beats normal weight.
fn face_score(face: &FaceInfo) -> u8 {
    let mut score = 0;
    if face.style != Style::Normal {
        score += 2;
    }
    if face.weight != Weight::NORMAL {
        score += 1;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_has_families_and_resolves() {
        let catalog = FontCatalog::new();

        // Any normal desktop has a few fonts.
        assert!(!catalog.families().is_empty(), "no system fonts found");

        // The first family is resolved to actual bytes.
        let first = catalog.families()[0].clone();
        assert!(catalog.has_family(&first));
        let resolved = catalog.resolve(&first).expect("first family must resolve");
        assert!(!resolved.data.is_empty());
    }

    #[test]
    fn resolve_is_case_insensitive_and_cached() {
        let catalog = FontCatalog::new();
        let Some(family) = catalog.families().first().cloned() else {
            return;
        };
        let lower = family.to_lowercase();
        assert!(catalog.resolve(&lower).is_some());
        assert!(catalog.resolve(&lower).is_some());
        assert!(
            catalog
                .resolve("definitely-not-installed-family-xyz")
                .is_none()
        );
    }

    #[test]
    fn typst_font_store_is_shared() {
        let catalog = FontCatalog::new();
        let a = catalog.build_typst_font_store();
        let b = catalog.build_typst_font_store();
        assert!(Arc::ptr_eq(&a, &b), "store must be cached, not rebuilt");
        assert!(a.book().families().next().is_some());
    }

    #[test]
    fn empty_catalog_still_has_embedded_fonts() {
        let catalog = FontCatalog::empty();
        assert!(catalog.families().is_empty());
        let store = catalog.build_typst_font_store();
        assert!(
            store.book().families().next().is_some(),
            "embedded fonts must remain available before the system scan lands"
        );
    }

    #[test]
    fn ready_fonts_are_visible_to_workers() {
        let fonts = Fonts::ready(FontCatalog::empty());
        assert!(fonts.is_ready());
        assert!(fonts.wait().families().is_empty());
    }

    #[test]
    fn started_fonts_eventually_become_ready() {
        let fonts = Fonts::start();
        let catalog = fonts.wait();
        assert!(fonts.is_ready());
        assert!(!catalog.families().is_empty(), "no system fonts found");
    }

    #[test]
    fn resolved_cache_evicts_the_oldest_beyond_the_cap() {
        let mut cache = ResolvedCache::default();
        for i in 0..RESOLVED_CACHE_CAP {
            cache.insert(format!("key-{i}"), None);
        }
        assert_eq!(cache.entries.len(), RESOLVED_CACHE_CAP);

        // Touch the oldest key so it becomes the most-recently used.
        cache.touch("key-0");

        cache.insert("overflow".to_owned(), None);
        assert_eq!(cache.entries.len(), RESOLVED_CACHE_CAP, "stays at the cap");
        assert!(cache.entries.contains_key("overflow"));
        assert!(
            !cache.entries.contains_key("key-1"),
            "the oldest untended key is evicted"
        );
        assert!(
            cache.entries.contains_key("key-0"),
            "the touched key survives"
        );
        assert_eq!(
            cache.order.len(),
            cache.entries.len(),
            "recency order and map stay in sync"
        );
    }

    #[test]
    fn resolve_stays_correct_while_the_cache_churns() {
        let catalog = FontCatalog::new();
        let Some(family) = catalog.families().first().cloned() else {
            return;
        };
        let first = catalog.resolve(&family).expect("first family resolves");

        // Overflow the cache with synthetic misses, forcing live entries out.
        {
            let mut cache = lock(&catalog.resolved);
            for i in 0..RESOLVED_CACHE_CAP + 5 {
                cache.insert(format!("synthetic-{i}"), None);
            }
        }
        assert_eq!(
            lock(&catalog.resolved).entries.len(),
            RESOLVED_CACHE_CAP,
            "synthetic overflow is bounded"
        );

        let again = catalog.resolve(&family).expect("the family still resolves");
        assert_eq!(again.data, first.data);
        assert_eq!(again.index, first.index);
    }
}
