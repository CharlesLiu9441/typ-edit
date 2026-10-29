//! Lexical extraction of i18n call-site keys from Rust source.

/// Scans Rust `text` for i18n call-site keys.
///
/// A key is the first argument of a `t` / `t_static` / `t_with` call when that
/// argument is a plain double-quoted string literal, allowing whitespace or
/// newlines anywhere between the identifier, `(`, and the quote. Qualified
/// calls such as `typ_edit_i18n::t_with(...)` are matched because the token
/// boundary before `t` is `:`. Comments and the contents of string, char, and
/// raw-string literals are skipped, so a `t("...")` that only appears in a
/// comment or inside another string is not a call site.
pub(super) fn literal_keys_in_source(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut keys = Vec::new();
    let mut i = 0;
    while i < len {
        let b = bytes[i];

        // Line comment (`//`, `///`, `//!`): skip to end of line.
        if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // Block comment (`/* ... */`). Rust block comments nest, but nested
        // block comments do not occur in these sources; a flat skip suffices.
        if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(len);
            continue;
        }

        // Raw string (`r"..."`, `r#"..."#`, `br"..."`).
        if let Some(end) = raw_string_end(bytes, i) {
            i = end;
            continue;
        }

        // Plain string literal.
        if b == b'"' {
            i = plain_string_end(bytes, i);
            continue;
        }

        // Char literal (`'"'`, `'a'`, `'\n'`) or a lifetime (`'a`). Only skip
        // genuine char literals so a `'"'` does not start a string.
        if b == b'\'' {
            match char_literal_end(bytes, i) {
                Some(end) => i = end,
                None => i += 1,
            }
            continue;
        }

        // Identifier: a candidate `t` / `t_static` / `t_with` call. The
        // preceding byte must not be word-like, so `format(` and `split(`
        // never match, while `::t(` and `.t(` (a path qualifier) do.
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            let mut j = i;
            while j < len && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            let prev_is_word =
                start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
            if !prev_is_word {
                let ident = &text[start..j];
                if matches!(ident, "t" | "t_static" | "t_with")
                    && let Some((key, end)) = first_string_arg(text, bytes, j)
                {
                    keys.push(key);
                    i = end;
                    continue;
                }
            }
            i = j;
            continue;
        }

        i += 1;
    }
    keys
}

/// If `i` (just past an identifier) starts a call whose first argument is a
/// plain string literal, returns the key and the index after the closing quote.
fn first_string_arg(text: &str, bytes: &[u8], mut i: usize) -> Option<(String, usize)> {
    let len = bytes.len();
    while i < len && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if bytes.get(i) != Some(&b'(') {
        return None;
    }
    i += 1;
    while i < len && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    let end = plain_string_end(bytes, i);
    let inner = text.get(i + 1..end.checked_sub(1)?)?;
    Some((inner.to_owned(), end))
}

/// Index just past the closing quote of the plain string starting at the `"`
/// at `start`. Handles `\"` escapes; unterminated input clamps to the end.
fn plain_string_end(bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    let mut i = start + 1;
    while i < len {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    len
}

/// If a raw string begins at `start`, returns the index just past its
/// terminator; otherwise `None`.
fn raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut p = start;
    if bytes.get(p) == Some(&b'b') {
        p += 1;
    }
    if bytes.get(p) != Some(&b'r') {
        return None;
    }
    p += 1;
    let mut hashes = 0;
    while bytes.get(p) == Some(&b'#') {
        hashes += 1;
        p += 1;
    }
    if bytes.get(p) != Some(&b'"') {
        return None;
    }
    p += 1;
    while p < len {
        if bytes[p] == b'"' {
            let mut q = p + 1;
            let mut seen = 0;
            while seen < hashes && bytes.get(q) == Some(&b'#') {
                seen += 1;
                q += 1;
            }
            if seen == hashes {
                return Some(q);
            }
        }
        p += 1;
    }
    Some(len)
}

/// If a char literal begins at `start`, returns the index just past its closing
/// quote; otherwise `None` (e.g. a lifetime like `'a`).
fn char_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    let i = start + 1;
    let ch_len = utf8_char_len(*bytes.get(i)?);
    let after_char = i + ch_len;
    if bytes.get(after_char) == Some(&b'\'') {
        Some(after_char + 1)
    } else {
        None
    }
}

/// Byte length of the UTF-8 character whose lead byte is `b`.
fn utf8_char_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// A literal call site yields its key.
#[test]
fn scanner_finds_literal_calls() {
    let src = "let a = t(\"menu.file.save\");";
    assert_eq!(literal_keys_in_source(src), vec!["menu.file.save"]);
}

/// Qualified calls and all three `t`-family names are matched.
#[test]
fn scanner_matches_qualified_and_family_names() {
    let src = "typ_edit_i18n::t_static(\"app.title\"); t_with(\"toast.saved\", &[]);";
    assert_eq!(
        literal_keys_in_source(src),
        vec!["app.title", "toast.saved"]
    );
}

/// A call split across lines is still found.
#[test]
fn scanner_handles_multiline_calls() {
    let src =
        "wrap(\n    t_with(\n        \"dialog.rename.failed\",\n        &[(\"err\", e)],\n    ));";
    assert_eq!(literal_keys_in_source(src), vec!["dialog.rename.failed"]);
}

/// Comment mentions are not call sites.
#[test]
fn scanner_skips_comments() {
    let src = "// t(\"comment.only\")\n/// t_static(\"doc.only\")\nlet x = 1; // t_with(\"trailing.only\")\n";
    assert!(literal_keys_in_source(src).is_empty(), "comments matched");
}

/// A non-literal first argument is not a key.
#[test]
fn scanner_skips_non_literal_arguments() {
    let src = "t(key_var); t_with(format!(\"x\"), &[]); t_static(choice.into());";
    assert!(literal_keys_in_source(src).is_empty());
}

/// `t(` text inside a string literal is not a call site.
#[test]
fn scanner_skips_string_contents() {
    let src = "let s = \"t(\\\"fake.inside.string\\\")\"; let r = r#\"t(\\\"raw.fake\\\")\"#;";
    assert!(literal_keys_in_source(src).is_empty());
}

/// Lookalike identifiers (`format`, `split`) are not calls to `t`.
#[test]
fn scanner_ignores_longer_identifiers() {
    let src = "format!(\"x\"); split(\"y\"); edit(\"z\");";
    assert!(literal_keys_in_source(src).is_empty());
}
