//! World-dependent font lints ported from tinymist's `rules/bad_font.rs`.
//!
//! Rule ⑤ warns about `font:` values that name a variable font (`…VF`), a pure
//! syntax check over `set text(...)` and `text(...)`. Rule ⑥ enriches the
//! compiler's own `unknown font family:` diagnostic with "did you mean"
//! suggestions: it never invents a diagnostic, it only appends a hint to an
//! existing one whose font string is close to an installed family name
//! (Jaro-Winkler similarity strictly above `0.6`, tinymist's threshold).
//!
//! Following the syntax lints, a hint is folded into the message as a trailing
//! `\nHint: {hint}` line and every diagnostic is a warning. Byte ranges are
//! UTF-8 offsets into `source`; `path` is left empty for the caller to fill in.

use std::ops::Range;
use std::path::PathBuf;

use typ_edit_model::{BytePos, Diag, Severity};
use typst::syntax::ast::{self, AstNode};
use typst::syntax::{LinkedNode, Source, Span, SpanKind};

/// Message of a variable-font warning, matching tinymist's wording.
const VARIABLE_FONT_MESSAGE: &str = "variable font is not supported by typst yet";

/// Hint appended to a variable-font warning, matching tinymist's wording.
const VARIABLE_FONT_HINT: &str = "consider using a static font instead. For more information, see https://github.com/typst/typst/issues/185";

/// Prefix of the compiler's unknown-font diagnostic; only those are enriched.
const UNKNOWN_FONT_PREFIX: &str = "unknown font family:";

/// Jaro-Winkler similarity above which two names count as similar.
const SIMILARITY_THRESHOLD: f64 = 0.6;

/// Maximum number of font suggestions appended to one diagnostic.
const MAX_SUGGESTIONS: usize = 3;

/// Rule ⑤: warns for every `font:` string that names a variable font.
///
/// A variable font is detected purely by the `VF` suffix (`ends_with("VF")`,
/// case-sensitive, like tinymist), so the string is inspected wherever it
/// appears as a `font:` argument: directly, as an array element, or as the
/// `name` value of a font dictionary.
pub fn variable_font_warnings(source: &Source) -> Vec<Diag> {
    font_strings(source)
        .into_iter()
        .filter(|font| font.value.ends_with("VF"))
        .map(|font| Diag {
            path: PathBuf::new(),
            range: BytePos(font.range.start)..BytePos(font.range.end),
            message: format!("{VARIABLE_FONT_MESSAGE}\nHint: {VARIABLE_FONT_HINT}"),
            severity: Severity::Warning,
        })
        .collect()
}

/// Rule ⑥: appends "did you mean" hints to the compiler's unknown-font
/// diagnostics.
///
/// Each diagnostic whose message starts with `unknown font family:` is matched
/// to the `font:` string its range points at; similar installed families are
/// looked up and, when any is found, a hint is appended. Diagnostics with no
/// similar family are left untouched, and no diagnostic is ever added.
pub fn augment_unknown_font(diags: &mut [Diag], source: &Source, families: &[String]) {
    let strings = font_strings(source);
    for diag in diags.iter_mut() {
        if !diag.message.starts_with(UNKNOWN_FONT_PREFIX) {
            continue;
        }
        // Prefer the AST string under the diagnostic; fall back to the family
        // name the compiler reported when the range points elsewhere.
        let query = match strings.iter().find(|font| {
            font.range.start <= diag.range.start.0 && diag.range.start.0 < font.range.end
        }) {
            Some(font) => font.value.clone(),
            None => diag.message[UNKNOWN_FONT_PREFIX.len()..].trim().to_owned(),
        };

        // A localized name maps to its installed English equivalents; anything
        // else falls back to string similarity against the installed list.
        let mapped: Vec<String> = font_family_names(&query)
            .into_iter()
            .filter(|name| {
                families
                    .iter()
                    .any(|family| family.eq_ignore_ascii_case(name))
            })
            .collect();
        let suggestions = if mapped.is_empty() {
            similar_fonts(&query, families, MAX_SUGGESTIONS)
        } else {
            mapped
        };
        if suggestions.is_empty() {
            continue;
        }
        diag.message.push_str(&suggestion_hint(&suggestions));
    }
}

/// Splits a font-value string into individual family names.
///
/// Names are split on commas, trimmed, unquoted, and localized names are
/// expanded to their English equivalents via [`LOCALIZED_FONT_MAPPING`];
/// empty parts are dropped.
pub fn font_family_names(value: &str) -> Vec<String> {
    let mut names = Vec::new();
    for part in value.split(',') {
        let name = part.trim().trim_matches(|c| c == '"' || c == '\'').trim();
        if name.is_empty() {
            continue;
        }
        match localized_names(name) {
            Some(english) => names.extend(english.iter().map(|name| (*name).to_owned())),
            None => names.push(name.to_owned()),
        }
    }
    names
}

/// The installed font families most similar to `query`.
///
/// Compares lowercased names with Jaro-Winkler similarity, keeps those strictly
/// above `0.6`, and returns up to `limit` of them ordered by descending
/// similarity and then by name.
pub fn similar_fonts(query: &str, families: &[String], limit: usize) -> Vec<String> {
    let query = query.to_lowercase();
    let mut scored: Vec<(&String, f64)> = families
        .iter()
        .map(|family| (family, strsim::jaro_winkler(&query, &family.to_lowercase())))
        .filter(|(_, score)| *score > SIMILARITY_THRESHOLD)
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(family, _)| family.clone())
        .collect()
}

/// A `font:` string argument: its byte range and the string's contents.
struct FontStr {
    range: Range<usize>,
    value: String,
}

/// Collects every string that is a `font:` argument of a `text` call.
fn font_strings(source: &Source) -> Vec<FontStr> {
    let mut fonts = Vec::new();
    walk(
        source,
        LinkedNode::new(source.root()),
        &mut |range, value| {
            fonts.push(FontStr {
                range,
                value: value.to_owned(),
            });
        },
    );
    fonts
}

/// Walks the tree, visiting the font strings of every `text` call or `set text`.
fn walk<'a>(source: &Source, node: LinkedNode<'a>, visit: &mut impl FnMut(Range<usize>, &str)) {
    if let Some(call) = node.cast::<ast::FuncCall>() {
        if call.callee().to_untyped().leaf_text().as_str() == "text" {
            font_args(source, call.args().items(), visit);
        }
    } else if let Some(set) = node.cast::<ast::SetRule>()
        && set.target().to_untyped().leaf_text().as_str() == "text"
    {
        font_args(source, set.args().items(), visit);
    }
    for child in node.children() {
        walk(source, child, visit);
    }
}

/// Visits every `font:` value among `args`.
fn font_args<'a>(
    source: &Source,
    args: impl Iterator<Item = ast::Arg<'a>>,
    visit: &mut impl FnMut(Range<usize>, &str),
) {
    for arg in args {
        let ast::Arg::Named(named) = arg else {
            continue;
        };
        if named.name().as_str() != "font" {
            continue;
        }
        font_expr(source, named.expr(), visit);
    }
}

/// Visits the strings of a `font:` value: the string itself, every array
/// element, and the `name` value of every font dictionary.
fn font_expr<'a>(source: &Source, expr: ast::Expr<'a>, visit: &mut impl FnMut(Range<usize>, &str)) {
    match expr {
        ast::Expr::Str(str_) => {
            if let Some(range) = span_range(source, str_.span()) {
                visit(range, str_.get().as_str());
            }
        }
        ast::Expr::Array(array) => {
            for item in array.items() {
                match item {
                    ast::ArrayItem::Pos(expr) => font_expr(source, expr, visit),
                    ast::ArrayItem::Spread(_) => {}
                }
            }
        }
        ast::Expr::Dict(dict) => {
            for item in dict.items() {
                if let ast::DictItem::Named(arg) = item
                    && arg.name().as_str() == "name"
                {
                    font_expr(source, arg.expr(), visit);
                }
            }
        }
        _ => {}
    }
}

/// Maps a syntax span to its byte range in `source`, or `None` when it points
/// elsewhere.
fn span_range(source: &Source, span: Span) -> Option<Range<usize>> {
    match span.get() {
        SpanKind::Number { id, num } if id == source.id() => source.range(num, None),
        SpanKind::Range { id, range } if id == source.id() => Some(range),
        _ => None,
    }
}

/// The English equivalents of a localized font name, if the table knows it.
fn localized_names(name: &str) -> Option<&'static [&'static str]> {
    LOCALIZED_FONT_MAPPING
        .iter()
        .find(|(localized, _)| *localized == name)
        .map(|(_, english)| *english)
}

/// The `\nHint: did you mean ...` line for a non-empty suggestion list,
/// matching tinymist's wording.
fn suggestion_hint(suggestions: &[String]) -> String {
    if suggestions.len() == 1 {
        format!("\nHint: did you mean '{}'?", suggestions[0])
    } else {
        format!(
            "\nHint: did you mean one of: '{}'?",
            suggestions.join("', '")
        )
    }
}

/// A mapping of common localized font names to their English equivalents,
/// ported verbatim from tinymist's `LOCALIZED_FONT_MAPPING`.
// allow: SIZE_OK — ported data table, not logic.
const LOCALIZED_FONT_MAPPING: &[(&str, &[&str])] = &[
    // Chinese Simplified
    ("宋体", &["SimSun"]),
    ("新宋体", &["NSimSun"]),
    ("黑体", &["SimHei"]),
    ("微软雅黑", &["Microsoft YaHei"]),
    ("楷体", &["KaiTi", "KaiTi_GB2312"]),
    ("仿宋", &["FangSong", "FangSong_GB2312"]),
    ("等线", &["DengXian"]),
    ("幼圆", &["YouYuan"]),
    ("华文宋体", &["STSong"]),
    ("华文楷体", &["STKaiti"]),
    ("华文仿宋", &["STFangsong"]),
    ("华文黑体", &["STHeiti"]),
    ("华文细黑", &["STXihei"]),
    // Chinese Traditional
    ("新細明體", &["PMingLiU"]),
    ("細明體", &["MingLiU"]),
    ("標楷體", &["DFKai-SB"]),
    ("微軟正黑體", &["Microsoft JhengHei"]),
    // Japanese
    ("ＭＳ 明朝", &["MS Mincho"]),
    ("ＭＳ Ｐ明朝", &["MS PMincho"]),
    ("ＭＳ ゴシック", &["MS Gothic"]),
    ("ＭＳ Ｐゴシック", &["MS PGothic"]),
    ("メイリオ", &["Meiryo"]),
    // Korean
    ("맑은 고딕", &["Malgun Gothic"]),
    ("바탕", &["Batang"]),
    ("돋움", &["Dotum"]),
    ("궁서", &["Gungsuh"]),
    ("굴림", &["Gulim"]),
];

#[cfg(test)]
mod tests {
    use super::*;
    use typ_edit_model::ByteRange;

    /// The byte range of `needle` inside `text`.
    fn range_of(text: &str, needle: &str) -> ByteRange {
        let start = text.find(needle).expect("needle in text");
        BytePos(start)..BytePos(start + needle.len())
    }

    /// A warning diagnostic at `range` with the given message.
    fn diag(range: ByteRange, message: &str) -> Diag {
        Diag {
            path: PathBuf::new(),
            range,
            message: message.to_owned(),
            severity: Severity::Warning,
        }
    }

    #[test]
    fn variable_font_names_warn_in_set_and_direct_call() {
        let text = "#set text(font: \"Noto Sans VF\")";
        let diags = variable_font_warnings(&Source::detached(text));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].range, range_of(text, "\"Noto Sans VF\""));
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(
            diags[0].path.as_os_str().is_empty(),
            "path is filled by caller"
        );
        assert_eq!(
            diags[0].message,
            "variable font is not supported by typst yet\nHint: consider using a static font \
             instead. For more information, see https://github.com/typst/typst/issues/185"
        );

        let text = "#text(font: \"Noto Sans VF\")[123]";
        let diags = variable_font_warnings(&Source::detached(text));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].range, range_of(text, "\"Noto Sans VF\""));
    }

    #[test]
    fn variable_font_array_and_dict_forms_warn() {
        let text = "#text(font: (\"Arial\", \"Noto Sans VF\"))[123]";
        let diags = variable_font_warnings(&Source::detached(text));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].range, range_of(text, "\"Noto Sans VF\""));

        let text = "#set text(font: ((name: \"Noto Sans VF\"),))";
        let diags = variable_font_warnings(&Source::detached(text));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].range, range_of(text, "\"Noto Sans VF\""));
    }

    #[test]
    fn static_fonts_and_unrelated_calls_do_not_warn() {
        assert!(
            variable_font_warnings(&Source::detached("#set text(font: \"DejaVu Sans Mono\")"))
                .is_empty()
        );
        // Only `text` takes a font family; another callee is not the rule's subject.
        assert!(
            variable_font_warnings(&Source::detached("#foo(font: \"Noto Sans VF\")")).is_empty()
        );
        // The suffix is case-sensitive, like tinymist.
        assert!(
            variable_font_warnings(&Source::detached("#set text(font: \"Noto Sans vf\")"))
                .is_empty()
        );
    }

    #[test]
    fn font_family_names_splits_trims_and_unquotes() {
        assert_eq!(
            font_family_names("Arial, Times New Roman"),
            ["Arial", "Times New Roman"]
        );
        assert_eq!(
            font_family_names(" \"Arial\" , 'Times New Roman' "),
            ["Arial", "Times New Roman"]
        );
        assert_eq!(font_family_names(", ,"), Vec::<String>::new());
        assert_eq!(font_family_names(""), Vec::<String>::new());
    }

    #[test]
    fn font_family_names_expands_localized_names() {
        assert_eq!(font_family_names("宋体"), ["SimSun"]);
        assert_eq!(font_family_names("楷体"), ["KaiTi", "KaiTi_GB2312"]);
        assert_eq!(font_family_names("宋体, Arial"), ["SimSun", "Arial"]);
    }

    #[test]
    fn similar_fonts_ranks_by_similarity_then_name_and_thresholds() {
        let families = vec![
            "New Computer Modern".to_owned(),
            "New Computer Modern Math".to_owned(),
            "Arial".to_owned(),
        ];
        assert_eq!(
            similar_fonts("New Computer Moder", &families, 3),
            ["New Computer Modern", "New Computer Modern Math"]
        );
        assert_eq!(similar_fonts("New Computer Moder", &families, 1).len(), 1);
        assert!(similar_fonts("zzzzzz", &families, 3).is_empty());
    }

    #[test]
    fn augment_unknown_font_adds_a_did_you_mean_hint() {
        let source = Source::detached("#set text(font: \"New Computer Moder\")");
        let range = range_of(source.text(), "\"New Computer Moder\"");
        let families = vec!["New Computer Modern".to_owned(), "Arial".to_owned()];
        let mut diags = vec![diag(range, "unknown font family: New Computer Moder")];

        augment_unknown_font(&mut diags, &source, &families);

        assert_eq!(diags.len(), 1, "no diagnostic is invented");
        assert_eq!(
            diags[0].message,
            "unknown font family: New Computer Moder\n\
             Hint: did you mean 'New Computer Modern'?"
        );
    }

    #[test]
    fn augment_unknown_font_maps_localized_names_to_installed_fonts() {
        let source = Source::detached("#set text(font: \"微软雅黑\")");
        let range = range_of(source.text(), "\"微软雅黑\"");
        let families = vec!["Microsoft YaHei".to_owned(), "Arial".to_owned()];
        let mut diags = vec![diag(range, "unknown font family: 微软雅黑")];

        augment_unknown_font(&mut diags, &source, &families);

        assert_eq!(
            diags[0].message,
            "unknown font family: 微软雅黑\nHint: did you mean 'Microsoft YaHei'?"
        );
    }

    #[test]
    fn augment_unknown_font_is_a_no_op_when_nothing_is_similar() {
        let source = Source::detached("#set text(font: \"zzzzzz\")");
        let range = range_of(source.text(), "\"zzzzzz\"");
        let families = vec!["New Computer Modern".to_owned()];
        let mut diags = vec![diag(range, "unknown font family: zzzzzz")];

        augment_unknown_font(&mut diags, &source, &families);
        assert_eq!(diags[0].message, "unknown font family: zzzzzz");

        // A diagnostic that is not the compiler's unknown-font one is untouched.
        let mut other = vec![diag(BytePos(0)..BytePos(1), "unknown variable: x")];
        augment_unknown_font(&mut other, &source, &families);
        assert_eq!(other[0].message, "unknown variable: x");
    }

    #[test]
    fn similar_fonts_matches_the_strsim_threshold() {
        // `strsim` scores this pair just above the 0.6 threshold; the handwritten
        // port this replaced scored it just below. Pinning the ecosystem-crate
        // behavior keeps the swap honest.
        let families = vec!["New Computer Modern Math".to_owned()];
        assert_eq!(
            similar_fonts("thequickbrownfoxjumpedovery", &families, 3),
            ["New Computer Modern Math"]
        );
    }
}
