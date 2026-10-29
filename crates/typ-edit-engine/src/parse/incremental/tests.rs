use typ_edit_model::ParseResult;

use super::IncrementalParser;
use crate::parse::run;

/// Every observable field of the incremental result must equal the
/// full-parse reference, except `generation` (stamped by the caller).
#[track_caller]
fn assert_equivalent(got: &ParseResult, reference: &ParseResult, context: &str) {
    assert_eq!(
        got.highlights, reference.highlights,
        "highlights: {context}"
    );
    assert_eq!(got.stats, reference.stats, "stats: {context}");
    assert_eq!(
        got.layout.code_ranges, reference.layout.code_ranges,
        "code ranges: {context}"
    );
    assert_eq!(
        got.layout.math_ranges, reference.layout.math_ranges,
        "math ranges: {context}"
    );
    assert_eq!(
        got.layout.blocked_ranges, reference.layout.blocked_ranges,
        "blocked ranges: {context}"
    );
    assert_eq!(
        got.layout.math_atoms, reference.layout.math_atoms,
        "math atoms: {context}"
    );
    assert_eq!(
        got.layout.list_lines, reference.layout.list_lines,
        "list lines: {context}"
    );
    assert_eq!(
        format!("{:?}", got.diagnostics),
        format!("{:?}", reference.diagnostics),
        "diagnostics: {context}"
    );
    assert_eq!(
        format!("{:?}", got.ide),
        format!("{:?}", reference.ide),
        "ide: {context}"
    );
    assert_eq!(got.folds, reference.folds, "folds: {context}");
    assert_eq!(got.symbols, reference.symbols, "symbols: {context}");
    assert_eq!(got.prose, reference.prose, "prose: {context}");
}

fn assert_step(parser: &mut IncrementalParser, text: &str, cursor: usize, context: &str) {
    let got = parser.parse(text.to_owned(), cursor, false);
    assert_equivalent(&got, &run(text, cursor), context);
}

/// Deterministic xorshift so failures are reproducible without a dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Byte offsets of every char boundary, plus the end; all are valid edit and
/// cursor positions.
fn positions(text: &str) -> Vec<usize> {
    text.char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect()
}

const SNIPPETS: &[&str] = &[
    "a", " ", "\n", "你好", "*", "_", "$", "`", "#", "let x = ", "\"s\"", "// c\n", "/* c */",
    "```", "==", "\\", "[]", "(", ")", "\n\n", "= H\n", "- item\n",
];

/// Applies `steps` random byte-safe edits to `seed`, checking the
/// incremental parser against a full parse after every one.
fn feed(parser: &mut IncrementalParser, rng: &mut Rng, steps: usize, seed: &str) {
    let mut text = seed.to_owned();
    let pos = positions(&text);
    assert_step(
        parser,
        &text,
        pos[rng.below(pos.len())],
        &format!("seed {seed:?}"),
    );

    for step in 0..steps {
        let pos = positions(&text);
        let (a, b) = (rng.below(pos.len()), rng.below(pos.len()));
        let (start, end) = if a <= b {
            (pos[a], pos[b])
        } else {
            (pos[b], pos[a])
        };
        match rng.below(3) {
            0 => text.replace_range(start..end, ""),
            1 => text.replace_range(start..end, SNIPPETS[rng.below(SNIPPETS.len())]),
            _ => text.insert_str(start, SNIPPETS[rng.below(SNIPPETS.len())]),
        }

        let pos = positions(&text);
        let cursor = pos[rng.below(pos.len())];
        assert_step(
            parser,
            &text,
            cursor,
            &format!("seed {seed:?} step {step}: {text:?}"),
        );
    }
}

#[test]
fn random_edits_match_full_parse() {
    let seeds = [
        "= Title\n\nHello *world* and _emph_.\n\n- item one\n  - item two\n\n// a comment\n#let f(x) = \"str\"\n\n$ x + 1 $ and $ y = 2 $\n\n```\nraw block\n```\n",
        "你好世界\n\n= 标题\n\n- 项目一\n+ 项目二\n\n`raw` and $inline$\n\n```typ\ncode\n```\n",
        "plain text only",
        "#set text(font: \"x\")\n#rect(fill: red)[content *strong*]\n// comment\n#pagebreak()\n",
        "```\ncode here\n```\ntext after",
        "#let x = 1 // trailing comment\n#let y = /* block */ 2\n",
        "<label> #import \"x.typ\": thing\n\n#figure(fill: |)",
        "#set page(margin: (x: 1cm, y: 2.2cm))\n#align(center)[c]\n#rect(inset: (top: 1pt, left: 2pt))\n#let (a: b) = c",
        "#let alpha = 1\n#let beta = alpha + 1\n#beta\n#let f(g) = g\n",
        "$sqrt(2) + 3.14$ and $sin(x)$ and $12pt$ and $a, b, c$\n#let f = $x_1^2 + 50%$\n",
        // The user's leaf-breakdown example: nested calls, commas, parens, and
        // the `)sqrt(` boundary all become distinct adjacent atoms.
        "$sqrt(2)+frac(sqrt(2),root(10,13))$\n",
        // Lint triggers, so the incremental diagnostics comparison also covers
        // the appended syntax lints.
        "#let f() = {\n  set text(red)\n  return 1\n}\n#break\n#for x in (1, 2) {\n  break\n}\n#return\n",
    ];
    for (i, seed) in seeds.iter().enumerate() {
        let mut parser = IncrementalParser::new();
        let mut rng = Rng::new(0x9E37_79B9_7F4A_7C15 ^ (i as u64));
        feed(&mut parser, &mut rng, 150, seed);
        assert!(
            parser.windowed_splices > 0,
            "seed {seed:?} never exercised the windowed splice path"
        );
    }
}

#[test]
fn prose_segments_match_full_parse_when_requested() {
    let mut parser = IncrementalParser::new();
    let text = "= Head\n\nHello *world* and $x^2$ plus `code` and @ref.\n\n- one\n- two\n";
    let got = parser.parse(text.to_owned(), 0, true);
    let reference = crate::parse::run_timed(text, 0, true).0;
    assert_equivalent(&got, &reference, "prose enabled");
    assert!(
        !got.prose.is_empty(),
        "prose-enabled parse must extract runs"
    );

    let unrequested = parser.parse(text.to_owned(), 0, false);
    assert!(
        unrequested.prose.is_empty(),
        "spell off must yield no prose"
    );
}

#[test]
fn insert_at_start_and_delete_to_empty() {
    let mut parser = IncrementalParser::new();
    let mut text = "= Head\n\nbody".to_owned();
    assert_step(&mut parser, &text, 0, "initial");

    text.insert(0, 'X');
    assert_step(&mut parser, &text, 0, "insert at 0");

    text.insert_str(0, "*bold* ");
    assert_step(&mut parser, &text, text.len(), "insert strong at 0");

    text.clear();
    assert_step(&mut parser, &text, 0, "delete to empty");

    text.push_str("fresh");
    assert_step(&mut parser, &text, 0, "grow from empty");
}

#[test]
fn edits_adjacent_to_raw_and_comment_keep_inheritance() {
    let mut parser = IncrementalParser::new();
    let mut text = "before ```\nraw *not strong*\n``` after".to_owned();
    assert_step(&mut parser, &text, 0, "raw initial");

    // Edit inside the raw body: the raw tag must survive for the block.
    let raw = text.find("raw").unwrap();
    text.insert(raw, 'x');
    assert_step(&mut parser, &text, raw, "edit inside raw");

    // Edit right after the closing fence.
    let close = text.find("``` after").unwrap() + 3;
    text.insert(close, '!');
    assert_step(&mut parser, &text, close, "edit after raw fence");

    // Comment inheritance: editing inside a comment keeps the Comment tag.
    let mut text = "#let x = 1 // hello world\nbody".to_owned();
    assert_step(&mut parser, &text, 0, "comment initial");
    let comment = text.find("hello").unwrap();
    text.replace_range(comment..comment + 5, "hi");
    assert_step(&mut parser, &text, comment, "edit inside comment");
}

#[test]
fn large_structural_change_and_file_switch() {
    let mut parser = IncrementalParser::new();
    assert_step(&mut parser, "hello", 0, "small seed");

    // Unbalanced delimiters force the parser's whole-tree fallback path.
    let broken = "#let x = (a + b";
    assert_step(&mut parser, broken, broken.len(), "unbalanced code");

    // A completely different document.
    let other = "= New file\n\n$ E = m c^2 $\n\n```rust\nfn main() {}\n```\n你好\n";
    assert_step(&mut parser, other, other.len(), "file switch");

    // Back to the previous shape.
    assert_step(&mut parser, "plain again", 0, "switch back");
}

#[test]
fn definition_set_change_recomputes_highlights_fully() {
    let mut parser = IncrementalParser::new();
    let mut text = "#let x = 1\n#let a = x".to_owned();
    assert_step(&mut parser, &text, 0, "initial");
    let before = parser.full_fallbacks;

    // Rename the binding; the reference now points at an undefined name, so a
    // window-only splice could not retag it.
    let x = text.find("x = 1").unwrap();
    text.replace_range(x..x + 1, "y");
    assert_step(&mut parser, &text, x, "rename binding");

    assert!(
        parser.full_fallbacks > before,
        "a changed definition set must force a full highlight recompute"
    );
}

#[test]
fn stable_definition_set_body_edit_splices() {
    let mut parser = IncrementalParser::new();
    let mut text = String::from("#let x = 1\n");
    for line in 0..200 {
        text.push_str(&format!("// filler line {line}\n"));
    }
    text.push_str("#let a = 2");
    assert_step(&mut parser, &text, 0, "initial");

    let before_fallbacks = parser.full_fallbacks;
    let before_splices = parser.windowed_splices;
    let body = text.find("filler line 100").unwrap();
    text.replace_range(body..body + 11, "filler LINE");
    assert_step(&mut parser, &text, body, "edit body");

    assert_eq!(
        parser.full_fallbacks, before_fallbacks,
        "a stable definition set must not trigger the full fallback"
    );
    assert!(
        parser.windowed_splices > before_splices,
        "a body-only edit must take the windowed splice path"
    );
}
