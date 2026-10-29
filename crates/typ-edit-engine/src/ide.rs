//! IDE worker: answers references / prepare-rename / rename queries against the
//! latest compiled world.
//!
//! The worker owns no state of its own: each query carries the [`CompileResult`]
//! it must be answered against, so a coalesced request always reads the world
//! that produced the cursor's document. Coalescing mirrors the spell and search
//! workers: a burst of queries costs at most one pass, and the last request
//! wins.
//!
//! The heavy lifting lives in [`typ_edit_analysis`]; this module is the seam
//! that supplies the compiler-backed resolver (a file + name-start byte → its
//! [`definition`](typst_ide::definition)) and enumerates the project's `.typ`
//! files.

// allow: SIZE_OK — one cohesive IDE worker (request handling, project scan and
// the single-thread spawn are inseparable here); the file is one public surface
// `spawn` plus its private helpers.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::latest::LatestReceiver;
use crate::results::CompileResult;
use crate::supervisor::{EngineHealth, Pipeline, supervise};
use crate::wake::Waker;
use crate::world::WorldState;
use crossbeam_channel::Sender;
use typ_edit_analysis::classify::{SyntaxClass, classify};
use typ_edit_analysis::refs::find_references;
use typ_edit_analysis::rename::{is_valid_ident, prepare_rename, rename_edits};
use typ_edit_model::{
    CharCount, CompletionItem, CompletionKind, FileEdits, IdeQuery, IdeRequest, IdeResponse,
    IdeResult, PrepareRename, ReferenceLocation,
};
use typst::syntax::ast;
use typst::syntax::{LinkedNode, Side, Source};
use typst_ide::Definition;
use typst_layout::PagedDocument;

/// One ide request paired with the compilation result it must be answered
/// against; the `Arc` keeps the world alive until the worker has read it.
pub(crate) type IdeWork = (IdeRequest, Arc<CompileResult>);

/// Memoized project file list, keyed by `(root, generation)`. The list is
/// re-walked only when the root or the compile generation changes; the per-file
/// source lookup stays per request.
#[derive(Default)]
struct ProjectIndexCache {
    root: PathBuf,
    generation: u64,
    files: Vec<PathBuf>,
}

/// Spawns the ide worker thread. Reads a latest-wins mailbox and is supervised;
/// the project-file cache is keyed by `(root, generation)` and safe to retain
/// across a restart.
pub fn spawn(
    req: LatestReceiver<IdeWork>,
    res_tx: Sender<IdeResult>,
    waker: Waker,
    health: Sender<EngineHealth>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("typst-ide".to_owned())
        .spawn(move || {
            let mut cache = ProjectIndexCache::default();
            supervise(Pipeline::Ide, &health, || {
                while let Ok((request, result)) = req.recv() {
                    let response = handle(&request, &result, &mut cache);
                    let result = IdeResult {
                        generation: request.generation,
                        response,
                    };
                    if res_tx.send(result).is_err() {
                        break;
                    }
                    waker.wake();
                }
            });
        })
        .expect("spawn ide worker")
}

/// Shared context for answering one request against a compiled world.
struct Ctx<'a> {
    world: &'a WorldState,
    document: &'a PagedDocument,
    root: &'a Path,
    generation: u64,
}

/// A cursor in one source file: its path and the editor's caret-side byte.
#[derive(Clone, Copy)]
struct Cursor<'a> {
    path: &'a Path,
    byte: usize,
}

impl<'a> Cursor<'a> {
    fn new(path: &'a Path, byte: usize) -> Self {
        Self { path, byte }
    }
}

/// Answers one query against `result`'s compiled world.
///
/// A query whose world or document is missing yields the empty response for its
/// variant; a query the cursor cannot be resolved for is answered the same way.
fn handle(
    request: &IdeRequest,
    result: &CompileResult,
    cache: &mut ProjectIndexCache,
) -> IdeResponse {
    let Some(world) = result.world.as_deref() else {
        return empty_response(&request.query);
    };
    // Completions answer from the world's source alone, so an incomplete `it.`
    // (whose compile produces no document) still suggests members.
    if let IdeQuery::Complete { path, byte } = &request.query {
        return IdeResponse::Completions(completions(
            world,
            result.document.as_deref(),
            Cursor::new(path, byte.0),
        ));
    }
    let Some(document) = result.document.as_deref() else {
        return empty_response(&request.query);
    };
    let ctx = Ctx {
        world,
        document,
        root: &request.root,
        generation: result.generation,
    };

    match &request.query {
        IdeQuery::References { path, byte } => IdeResponse::References(
            references(&ctx, Cursor::new(path, byte.0), cache).unwrap_or_default(),
        ),
        IdeQuery::PrepareRename { path, byte } => {
            IdeResponse::PrepareRename(prepare(&ctx, Cursor::new(path, byte.0)))
        }
        IdeQuery::Rename {
            path,
            byte,
            new_name,
        } => IdeResponse::Rename(rename(&ctx, Cursor::new(path, byte.0), new_name, cache)),
        // Handled above, before the document requirement.
        IdeQuery::Complete { .. } => IdeResponse::Completions(Vec::new()),
    }
}

/// The empty answer for a query, used when there is no world to answer from.
fn empty_response(query: &IdeQuery) -> IdeResponse {
    match query {
        IdeQuery::References { .. } => IdeResponse::References(Vec::new()),
        IdeQuery::PrepareRename { .. } => IdeResponse::PrepareRename(None),
        IdeQuery::Rename { .. } => IdeResponse::Rename(None),
        IdeQuery::Complete { .. } => IdeResponse::Completions(Vec::new()),
    }
}

/// World-backed completions at `cursor`, mapped to the editor's wire type.
///
/// The cursor byte is the editor's caret byte; `typst_ide::autocomplete`
/// selects the leaf before it, which is the dot of a field access (`it.|`) or
/// the partial member (`it.ch|`). The returned `from` byte is discarded: the
/// editor recomputes the member word start from its own authoritative text, so
/// the popup never depends on a possibly stale worker byte. A missing source or
/// an unanswerable cursor yields an empty list (never a panic).
fn completions(
    world: &WorldState,
    document: Option<&PagedDocument>,
    cursor: Cursor,
) -> Vec<CompletionItem> {
    let Some(source) = world.source_of(cursor.path) else {
        return Vec::new();
    };
    let byte = clamp_byte(&source, cursor.byte);
    let Some((_, items)) = typst_ide::autocomplete(world, document, &source, byte, true) else {
        return Vec::new();
    };
    items.into_iter().map(completion_item).collect()
}

/// Largest char boundary at or before `byte`, clamped to the source length.
/// `autocomplete` slices the source text at the cursor, so the byte must not
/// split a multi-byte char.
fn clamp_byte(source: &Source, byte: usize) -> usize {
    let mut byte = byte.min(source.text().len());
    while byte > 0 && !source.text().is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

/// Maps one compiler completion onto the editor's candidate shape.
///
/// The compiler's `apply` may carry a snippet placeholder (`has(${})`,
/// `func()${}`); the editor has no snippet engine, so [`snippet_insert`]
/// converts it to plain text plus a caret offset. Without `apply` the label is
/// inserted.
fn completion_item(completion: typst_ide::Completion) -> CompletionItem {
    let typst_ide::Completion {
        kind,
        label,
        apply,
        detail,
    } = completion;
    let name = label.to_string();
    let (insert, cursor_back) = apply
        .as_deref()
        .map_or_else(|| (name.clone(), 0), snippet_insert);
    CompletionItem {
        insert,
        name,
        kind: completion_kind(&kind),
        cursor_back: CharCount(cursor_back),
        docs: detail.map(|detail| detail.to_string()),
    }
}

/// Converts compiler snippet text into plain insert text and a caret offset.
///
/// A `${...}` marks the caret placeholder; its inner text is inserted and the
/// caret lands before the trailing text (`has(${})` -> `has()` with the caret
/// inside). Text without a placeholder is inserted verbatim with no offset.
fn snippet_insert(apply: &str) -> (String, usize) {
    let Some(start) = apply.find("${") else {
        return (apply.to_owned(), 0);
    };
    let Some(rel_end) = apply[start + 2..].find('}') else {
        return (apply.to_owned(), 0);
    };
    let end = start + 2 + rel_end;
    let mut insert = String::with_capacity(apply.len());
    insert.push_str(&apply[..start]);
    insert.push_str(&apply[start + 2..end]);
    insert.push_str(&apply[end + 1..]);
    (insert, apply[end + 1..].chars().count())
}

/// Maps a compiler completion kind onto the editor's tag, keeping the closest
/// existing kind (the editor has no `path`/`font`/`package` tags).
fn completion_kind(kind: &typst_ide::CompletionKind) -> CompletionKind {
    match kind {
        typst_ide::CompletionKind::Syntax => CompletionKind::Keyword,
        typst_ide::CompletionKind::Func => CompletionKind::Function,
        typst_ide::CompletionKind::Type => CompletionKind::Type,
        typst_ide::CompletionKind::Param => CompletionKind::Param,
        typst_ide::CompletionKind::Constant => CompletionKind::Value,
        typst_ide::CompletionKind::Path => CompletionKind::Value,
        typst_ide::CompletionKind::Package => CompletionKind::Value,
        typst_ide::CompletionKind::Label => CompletionKind::Label,
        typst_ide::CompletionKind::Font => CompletionKind::Value,
        typst_ide::CompletionKind::Symbol(_) => CompletionKind::Symbol,
    }
}

/// Every reference to the item at `cursor`, sorted by path then start.
///
/// The cursor byte is the editor's caret-side byte; the resolver is driven from
/// the identifier's *start* byte (see [`resolve`]).
fn references(
    ctx: &Ctx,
    cursor: Cursor,
    cache: &mut ProjectIndexCache,
) -> Option<Vec<ReferenceLocation>> {
    let source = ctx.world.source_of(cursor.path)?;
    let (target, is_label, name) = target_at(ctx, &source, cursor)?;
    let files = project_sources(ctx, cache);
    let mut resolver = |path: &Path, byte: usize| resolve(ctx, path, byte);
    Some(find_references(
        &mut resolver,
        &files,
        &target,
        is_label,
        &name,
    ))
}

/// The renameable target under `cursor`, or `None` when the cursor is not on a
/// variable/label/reference this worker acts on.
fn target_at(ctx: &Ctx, source: &Source, cursor: Cursor) -> Option<(Definition, bool, String)> {
    match classify(source, cursor.byte)? {
        SyntaxClass::Label(node) => {
            let name = label_name(&node)?;
            Some((Definition::Span(node.span()), true, name))
        }
        SyntaxClass::Ref(node) => {
            let name = ref_name(&node)?;
            Some((Definition::Span(node.span()), true, name))
        }
        SyntaxClass::VarAccess(node) | SyntaxClass::Callee(node) => {
            let name = node.leaf_text().to_string();
            // `node.offset()` is the identifier start; resolution needs it (not
            // the click byte) so `resolve` can select the right leaf.
            let target = resolve(ctx, cursor.path, node.offset())?;
            Some((target, false, name))
        }
        SyntaxClass::ImportPath(_) | SyntaxClass::Other => None,
    }
}

/// Describes what a rename at `cursor` would change, or `None` when the cursor
/// is not renameable. Package files are classified by the request root.
fn prepare(ctx: &Ctx, cursor: Cursor) -> Option<PrepareRename> {
    let source = ctx.world.source_of(cursor.path)?;
    let is_package = !cursor.path.starts_with(ctx.root);
    let mut resolver = |p: &Path, b: usize| resolve(ctx, p, b);
    prepare_rename(&source, cursor.path, cursor.byte, is_package, &mut resolver).ok()
}

/// Groups the edits renaming the item at `cursor` to `new_name`, or `None` when
/// the name is invalid or the cursor is not renameable.
fn rename(
    ctx: &Ctx,
    cursor: Cursor,
    new_name: &str,
    cache: &mut ProjectIndexCache,
) -> Option<Vec<FileEdits>> {
    if !is_valid_ident(new_name) {
        return None;
    }
    // Prepare first: its refusals (package, field access, builtin, ...) gate the
    // rename before any project scan.
    prepare(ctx, cursor)?;
    let references = references(ctx, cursor, cache)?;
    Some(rename_edits(&references, new_name))
}

/// Resolves the occurrence that *begins* at `byte` in `path`.
///
/// `Side::After` at an identifier's start byte selects that identifier's leaf;
/// `Side::Before` would select the preceding leaf instead. (The click-byte
/// `definition_at` in [`CompileResult`] uses `Side::Before`; reference scanning
/// passes name-start bytes and therefore uses `Side::After`.)
fn resolve(ctx: &Ctx, path: &Path, byte: usize) -> Option<Definition> {
    let source = ctx.world.source_of(path)?;
    typst_ide::definition(ctx.world, Some(ctx.document), &source, byte, Side::After)
}

/// The name of a label node, or `None` when it does not cast.
fn label_name(node: &LinkedNode) -> Option<String> {
    node.cast::<ast::Label>()
        .map(|label| label.get().to_owned())
}

/// The target name of a reference node, or `None` when it does not cast.
fn ref_name(node: &LinkedNode) -> Option<String> {
    node.cast::<ast::Ref>()
        .map(|reference| reference.target().to_owned())
}

/// Every `.typ` file under the context's root, sorted, as `(path, source)`
/// pairs whose source the compiled world can supply. The path list is reused
/// from `cache` while `(root, generation)` is unchanged.
fn project_sources(ctx: &Ctx, cache: &mut ProjectIndexCache) -> Vec<(PathBuf, Source)> {
    if cache.root.as_path() != ctx.root || cache.generation != ctx.generation {
        cache.root = ctx.root.to_path_buf();
        cache.generation = ctx.generation;
        cache.files = project_files(ctx.root);
    }
    cache
        .files
        .iter()
        .filter_map(|path| {
            ctx.world
                .source_of(path)
                .map(|source| (path.clone(), source))
        })
        .collect()
}

/// Recursively collects every `.typ` file under `root`, mirroring the search
/// worker's walk policy: hidden entries and symlinks are skipped, the extension
/// check is case-insensitive, and unreadable subtrees are skipped rather than
/// aborting.
fn project_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_typ_files(root, &mut files);
    files.sort();
    files
}

/// Appends the `.typ` files in `dir` (recursively) to `out`.
fn collect_typ_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // `file_type` does not follow symlinks; skip them so a link cycle cannot
        // spin the walk.
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_typ_files(&entry.path(), out);
        } else if file_type.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("typ"))
        {
            out.push(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::FontCatalog;
    use std::time::SystemTime;
    use typ_edit_model::BytePos;

    /// A unique temp directory for one test, mirroring the other engine tests.
    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "typ-edit-ide-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Writes `contents` to `relative` under `root`, creating parent dirs.
    fn write_file(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&path, contents).expect("write file");
    }

    /// Writes `files` to a temp root, compiles `main`, and returns the result
    /// plus the root so the caller can address the project's files.
    fn compile_project(files: &[(&str, &str)], main: &str) -> (CompileResult, PathBuf) {
        let root = temp_dir("proj");
        for (relative, contents) in files {
            write_file(&root, relative, contents);
        }
        let main_path = root.join(main);
        let fonts = FontCatalog::new();
        let world = WorldState::new(
            root.clone(),
            main_path,
            Vec::new(),
            &fonts,
            Arc::new(crate::packages::system_packages()),
        );
        let document = typst::compile::<PagedDocument>(&world)
            .output
            .ok()
            .map(Arc::new);
        let page_count = document
            .as_ref()
            .map_or(0, |document| document.pages().len());
        let result = CompileResult {
            generation: 1,
            document,
            world: Some(Arc::new(world)),
            diagnostics: Vec::new(),
            error: None,
            page_count,
            missing: Vec::new(),
        };
        (result, root)
    }

    /// Runs one query through [`handle`] without threads.
    fn query(result: &CompileResult, root: &Path, query: IdeQuery) -> IdeResponse {
        let request = IdeRequest {
            generation: result.generation,
            root: root.to_path_buf(),
            query,
        };
        handle(&request, result, &mut ProjectIndexCache::default())
    }

    /// The end byte of the first occurrence of `needle` in `text` (the editor's
    /// caret-side byte).
    fn click_byte(text: &str, needle: &str) -> usize {
        text.find(needle).expect("needle in text") + needle.len()
    }

    #[test]
    fn cross_file_references_and_rename() {
        let a = "#let x = 41";
        let b = "#import \"a.typ\": x\n#x";
        let main = "#import \"b.typ\": x\n#x";
        let (result, root) = compile_project(
            &[("a.typ", a), ("b.typ", b), ("main.typ", main)],
            "main.typ",
        );
        let path_a = root.join("a.typ");
        let byte = click_byte(a, "x");

        let references = match query(
            &result,
            &root,
            IdeQuery::References {
                path: path_a.clone(),
                byte: BytePos(byte),
            },
        ) {
            IdeResponse::References(references) => references,
            other => panic!("expected references, got {other:?}"),
        };
        // Every occurrence that resolves to `a`'s `x`, including the import
        // bindings (which are renamed alongside the uses they feed).
        assert_eq!(
            references.len(),
            5,
            "a def, b import+use, main import+use: {references:?}"
        );
        let x = a.find('x').expect("x in a.typ");
        assert_eq!(references[0].path, path_a);
        assert_eq!(references[0].range, BytePos(x)..BytePos(x + 1));
        assert_eq!(
            references
                .iter()
                .filter(|r| r.path == root.join("b.typ"))
                .count(),
            2,
            "definition + use in b.typ: {references:?}"
        );
        assert_eq!(
            references
                .iter()
                .filter(|r| r.path == root.join("main.typ"))
                .count(),
            2,
            "import binding + use in main.typ: {references:?}"
        );

        let prepared = match query(
            &result,
            &root,
            IdeQuery::PrepareRename {
                path: path_a.clone(),
                byte: BytePos(byte),
            },
        ) {
            IdeResponse::PrepareRename(prepared) => prepared,
            other => panic!("expected prepare rename, got {other:?}"),
        }
        .expect("the definition is renameable");
        assert_eq!(prepared.placeholder, "x");
        assert!(!prepared.is_label);
        assert_eq!(prepared.range, BytePos(x)..BytePos(x + 1));

        let edits = match query(
            &result,
            &root,
            IdeQuery::Rename {
                path: path_a,
                byte: BytePos(byte),
                new_name: "y".to_owned(),
            },
        ) {
            IdeResponse::Rename(edits) => edits,
            other => panic!("expected rename edits, got {other:?}"),
        }
        .expect("rename yields edits");
        assert_eq!(edits.len(), 3, "one file each: {edits:?}");
        let per_file: Vec<usize> = edits.iter().map(|file| file.edits.len()).collect();
        assert_eq!(per_file, vec![1, 2, 2], "{edits:?}");
        for file in &edits {
            assert!(
                file.edits.iter().all(|edit| edit.new_text == "y"),
                "{file:?}"
            );
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rename_edits_only_the_outer_binding_under_shadowing() {
        let text = "#let x = 1\n#x\n#{\n  let x = 2\n  x\n}\n#x";
        let (result, root) = compile_project(&[("main.typ", text)], "main.typ");
        let path = root.join("main.typ");
        let byte = click_byte(text, "x");

        let edits = match query(
            &result,
            &root,
            IdeQuery::Rename {
                path,
                byte: BytePos(byte),
                new_name: "y".to_owned(),
            },
        ) {
            IdeResponse::Rename(edits) => edits,
            other => panic!("expected rename edits, got {other:?}"),
        }
        .expect("the outer binding is renameable");

        assert_eq!(edits.len(), 1, "all edits live in one file: {edits:?}");
        // The three outer occurrences: the binding and the two `#x` uses.
        let starts: Vec<usize> = edits[0].edits.iter().map(|e| e.range.start.0).collect();
        let outer_let = text.find("x = 1").expect("outer let");
        let first_use = text.find("\n#x").expect("first use") + 2;
        let last_use = text.rfind("#x").expect("last use") + 1;
        assert_eq!(starts, vec![outer_let, first_use, last_use], "{edits:?}");
        // The shadowed binding and its use are untouched.
        let shadowed_let = text.find("let x = 2").expect("shadowed let") + 4;
        assert!(
            !starts.contains(&shadowed_let),
            "the shadowed binding must not be renamed: {edits:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn labels_resolve_by_name_and_rename_only_the_name() {
        let text = "#figure[Body] <lab>\n\nSee @lab.";
        let (result, root) = compile_project(&[("main.typ", text)], "main.typ");
        let path = root.join("main.typ");

        // `classify` resolves a label from the byte at the name's end; the
        // figure's label needs an element to attach to for the document to
        // compile, and the ref needs it to exist.
        let label_name = text.find("lab>").expect("label name");
        let label_byte = label_name + "lab".len();
        let ref_name = text.find("@lab").expect("ref") + 1;

        let references = match query(
            &result,
            &root,
            IdeQuery::References {
                path: path.clone(),
                byte: BytePos(label_byte),
            },
        ) {
            IdeResponse::References(references) => references,
            other => panic!("expected references, got {other:?}"),
        };
        assert_eq!(references.len(), 2, "label and ref: {references:?}");
        assert_eq!(
            references[0].range,
            BytePos(label_name)..BytePos(label_name + 3),
            "name excludes `<>`"
        );
        assert_eq!(
            references[1].range,
            BytePos(ref_name)..BytePos(ref_name + 3),
            "name excludes `@`"
        );

        let prepared = match query(
            &result,
            &root,
            IdeQuery::PrepareRename {
                path: path.clone(),
                byte: BytePos(label_byte),
            },
        ) {
            IdeResponse::PrepareRename(prepared) => prepared,
            other => panic!("expected prepare rename, got {other:?}"),
        }
        .expect("a label is renameable");
        assert!(prepared.is_label);
        assert_eq!(prepared.placeholder, "lab");
        assert_eq!(prepared.range, BytePos(label_name)..BytePos(label_name + 3));

        let edits = match query(
            &result,
            &root,
            IdeQuery::Rename {
                path,
                byte: BytePos(label_byte),
                new_name: "other".to_owned(),
            },
        ) {
            IdeResponse::Rename(edits) => edits,
            other => panic!("expected rename edits, got {other:?}"),
        }
        .expect("label rename yields edits");
        let starts: Vec<usize> = edits[0].edits.iter().map(|e| e.range.start.0).collect();
        assert_eq!(starts, vec![label_name, ref_name], "{edits:?}");
        assert!(
            edits[0].edits.iter().all(|edit| edit.new_text == "other"),
            "{edits:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn builtins_and_field_accesses_are_not_renameable() {
        let text = "#let a = (b: 1)\n#a.b\n#text[hi]";
        let (result, root) = compile_project(&[("main.typ", text)], "main.typ");
        let path = root.join("main.typ");

        // A standard-library callee (`#text`) is refused.
        let text_byte = click_byte(text, "text");
        let builtin = query(
            &result,
            &root,
            IdeQuery::PrepareRename {
                path: path.clone(),
                byte: BytePos(text_byte),
            },
        );
        assert!(
            matches!(builtin, IdeResponse::PrepareRename(None)),
            "builtin must be refused: {builtin:?}"
        );

        // The field of a field access (`a.b`) is refused.
        let field_byte = click_byte(text, "a.b");
        let field = query(
            &result,
            &root,
            IdeQuery::PrepareRename {
                path: path.clone(),
                byte: BytePos(field_byte),
            },
        );
        assert!(
            matches!(field, IdeResponse::PrepareRename(None)),
            "field access must be refused: {field:?}"
        );

        // An invalid new name refuses the rename even on a renameable cursor.
        let a_byte = text.find("a = ").expect("a binding") + 1;
        for new_name in ["let", "1x"] {
            let renamed = query(
                &result,
                &root,
                IdeQuery::Rename {
                    path: path.clone(),
                    byte: BytePos(a_byte),
                    new_name: new_name.to_owned(),
                },
            );
            assert!(
                matches!(renamed, IdeResponse::Rename(None)),
                "`{new_name}` must be refused: {renamed:?}"
            );
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn paths_outside_the_root_are_refused() {
        let text = "#let x = 1\n#x";
        let (result, root) = compile_project(&[("main.typ", text)], "main.typ");
        let outside = std::env::temp_dir().join("typ-edit-ide-outside.typ");

        let prepared = query(
            &result,
            &root,
            IdeQuery::PrepareRename {
                path: outside,
                byte: BytePos(1),
            },
        );
        assert!(
            matches!(prepared, IdeResponse::PrepareRename(None)),
            "a path outside the root must be refused: {prepared:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn worker_round_trips_a_query_through_the_mailbox() {
        let text = "#let x = 1\n#x";
        let (result, root) = compile_project(&[("main.typ", text)], "main.typ");
        let (wake_tx, wake_rx) = crossbeam_channel::unbounded();
        let waker: Waker = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        let (req_tx, req_rx) = crate::latest::latest::<IdeWork>();
        let (res_tx, res_rx) = crossbeam_channel::bounded(16);
        let (health_tx, _health_rx) = crossbeam_channel::bounded(32);
        let handle = spawn(req_rx, res_tx, waker, health_tx);

        req_tx.send((
            IdeRequest {
                generation: result.generation,
                root: root.clone(),
                query: IdeQuery::References {
                    path: root.join("main.typ"),
                    byte: BytePos(click_byte(text, "x")),
                },
            },
            Arc::new(result),
        ));
        let answer = res_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("worker answers");
        assert_eq!(answer.generation, 1);
        match answer.response {
            IdeResponse::References(references) => {
                assert_eq!(references.len(), 2, "{references:?}");
            }
            other => panic!("expected references, got {other:?}"),
        }
        wake_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the worker wakes the UI");

        drop(req_tx);
        let _ = handle.join();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn project_index_cache_reuses_files_until_the_generation_changes() {
        let text = "#let x = 1\n#x";
        let (result, root) = compile_project(&[("main.typ", text)], "main.typ");
        let world = result.world.as_deref().expect("compiled world");
        let document = result.document.as_deref().expect("compiled document");
        let mut cache = ProjectIndexCache::default();

        let ctx = Ctx {
            world,
            document,
            root: root.as_path(),
            generation: 1,
        };
        let first = project_sources(&ctx, &mut cache);
        assert!(!first.is_empty());
        assert_eq!(cache.files, vec![root.join("main.typ")]);

        std::fs::write(root.join("extra.typ"), "= Extra").expect("write extra file");
        let _ = project_sources(&ctx, &mut cache);
        assert_eq!(
            cache.files,
            vec![root.join("main.typ")],
            "the same (root, generation) reuses the walked file list"
        );

        let ctx = Ctx {
            world,
            document,
            root: root.as_path(),
            generation: 2,
        };
        let _ = project_sources(&ctx, &mut cache);
        assert_eq!(
            cache.files,
            vec![root.join("extra.typ"), root.join("main.typ")],
            "a new generation re-walks the project"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn completes_dict_members_from_the_compiled_world() {
        let text = "#let d = (alpha: 1, beta: 2)\n#d.alpha";
        let (result, root) = compile_project(&[("main.typ", text)], "main.typ");
        let path = root.join("main.typ");
        // Right after the dot, before the already-present `alpha`: the field
        // access is complete enough for the document to compile, and the cursor
        // still sits in the member position `d.|`.
        let byte = text.find("#d.").expect("#d. in text") + 3;

        let items = match query(
            &result,
            &root,
            IdeQuery::Complete {
                path,
                byte: BytePos(byte),
            },
        ) {
            IdeResponse::Completions(items) => items,
            other => panic!("expected completions, got {other:?}"),
        };
        let names: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
        assert!(names.contains(&"alpha"), "dict member missing: {names:?}");
        assert!(names.contains(&"beta"), "dict member missing: {names:?}");
        let alpha = items
            .iter()
            .find(|item| item.name == "alpha")
            .expect("alpha completion");
        assert_eq!(alpha.insert, "alpha", "a plain member inserts its label");
        assert_eq!(alpha.cursor_back, CharCount(0));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn completes_content_members_in_a_show_rule() {
        let text = "#show list: it => {\n  it.\n}\n#list[[a]]";
        let (result, root) = compile_project(&[("main.typ", text)], "main.typ");
        let path = root.join("main.typ");
        let byte = text.find("it.").expect("it. in text") + 3;

        let items = match query(
            &result,
            &root,
            IdeQuery::Complete {
                path,
                byte: BytePos(byte),
            },
        ) {
            IdeResponse::Completions(items) => items,
            other => panic!("expected completions, got {other:?}"),
        };
        let names: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
        assert!(
            names.contains(&"children"),
            "content member `children` missing: {names:?}"
        );
        assert!(
            names.contains(&"indent"),
            "content member `indent` missing: {names:?}"
        );
        let children = items
            .iter()
            .find(|item| item.name == "children")
            .expect("children completion");
        assert_eq!(children.insert, "children");
        assert_eq!(children.cursor_back, CharCount(0));
        // A method's snippet placeholder becomes a plain call with the caret
        // inside the parentheses.
        let has = items
            .iter()
            .find(|item| item.name == "has")
            .expect("has method completion");
        assert_eq!(has.insert, "has()", "snippet placeholder must be stripped");
        assert_eq!(
            has.cursor_back,
            CharCount(1),
            "the caret lands inside the call"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn completion_kinds_map_onto_editor_kinds() {
        for (kind, expected) in [
            (typst_ide::CompletionKind::Syntax, CompletionKind::Keyword),
            (typst_ide::CompletionKind::Func, CompletionKind::Function),
            (typst_ide::CompletionKind::Type, CompletionKind::Type),
            (typst_ide::CompletionKind::Param, CompletionKind::Param),
            (typst_ide::CompletionKind::Constant, CompletionKind::Value),
            (typst_ide::CompletionKind::Path, CompletionKind::Value),
            (typst_ide::CompletionKind::Package, CompletionKind::Value),
            (typst_ide::CompletionKind::Label, CompletionKind::Label),
            (typst_ide::CompletionKind::Font, CompletionKind::Value),
            (
                typst_ide::CompletionKind::Symbol("x".into()),
                CompletionKind::Symbol,
            ),
        ] {
            assert_eq!(completion_kind(&kind), expected, "{kind:?}");
        }
    }

    #[test]
    fn missing_source_yields_empty_completions() {
        let text = "#let x = 1";
        let (result, root) = compile_project(&[("main.typ", text)], "main.typ");
        let outside = std::env::temp_dir().join("typ-edit-ide-complete-outside.typ");
        let items = match query(
            &result,
            &root,
            IdeQuery::Complete {
                path: outside,
                byte: BytePos(5),
            },
        ) {
            IdeResponse::Completions(items) => items,
            other => panic!("expected completions, got {other:?}"),
        };
        assert!(items.is_empty(), "a missing source suggests nothing");
        std::fs::remove_dir_all(&root).ok();
    }
}
