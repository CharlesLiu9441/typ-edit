//! Unicode character classification shared by the parse, spell and editor paths.

/// Whether `c` is a CJK/Kana/Hangul character that lays out and counts as its
/// own unit.
///
/// This is the **canonical union** of the three per-crate ranges the parse,
/// spell and editor code used to carry (P5-d of the architecture review): Han
/// ideographs (incl. extensions and compatibility forms), kana, Hangul
/// syllables, the CJK radicals/symbols/punctuation block and fullwidth forms.
/// Every path treats CJK as one class, so a character that soft-wraps
/// break-anywhere also counts as its own word and never reaches the spell
/// lexer.
///
/// Each range is inclusive; the union is a strict superset of the narrower
/// parse and editor sets, which is the intended unification.
pub fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{2E80}'..='\u{303F}'     // CJK radicals supp. + Kangxi + CJK symbols/punctuation
            | '\u{3040}'..='\u{30FF}' // Hiragana + Katakana
            | '\u{3400}'..='\u{4DBF}' // CJK Unified Ideographs Extension A
            | '\u{4E00}'..='\u{9FFF}' // CJK Unified Ideographs
            | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
            | '\u{AC00}'..='\u{D7AF}' // Hangul syllables
            | '\u{FF00}'..='\u{FFEF}' // Halfwidth/Fullwidth Forms
            | '\u{20000}'..='\u{2FA1F}' // CJK Extensions B–F + Compatibility Supplement
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character of the canonical union classifies as CJK.
    #[test]
    fn classifies_each_range() {
        for c in [
            '\u{2E80}',  // lower bound: CJK Radicals Supplement
            '\u{2F00}',  // Kangxi Radicals
            '\u{3002}',  // CJK Symbols and Punctuation: 。
            '\u{303F}',  // upper bound of the radicals/symbols block
            '\u{3040}',  // lower bound: Hiragana
            '\u{30FF}',  // upper bound: Katakana
            '\u{3400}',  // lower bound: Extension A
            '\u{4DBF}',  // upper bound: Extension A
            '\u{4E00}',  // lower bound: CJK Unified Ideographs
            '\u{9FFF}',  // upper bound: CJK Unified Ideographs
            '\u{F900}',  // lower bound: Compatibility Ideographs
            '\u{FAFF}',  // upper bound: Compatibility Ideographs
            '\u{AC00}',  // lower bound: Hangul syllables
            '\u{D7AF}',  // upper bound: Hangul syllables
            '\u{FF01}',  // Halfwidth/Fullwidth Forms: ！
            '\u{FFEF}',  // upper bound: Halfwidth/Fullwidth Forms
            '\u{20000}', // lower bound: Extensions B–F
            '\u{2FA1F}', // upper bound: Compatibility Supplement
        ] {
            assert!(is_cjk(c), "U+{:04X} must be CJK", c as u32);
        }
    }

    /// The whitespace/gaps around every range are not CJK.
    #[test]
    fn rejects_chars_outside_the_ranges() {
        for c in [
            '\u{2E7F}',  // just below the radicals block
            '\u{3100}',  // Bopomofo, between kana and Extension A
            '\u{3131}',  // Hangul Compatibility Jamo, same gap
            '\u{4DC0}',  // Yijing Hexagram Symbols, between Ext-A and Unified
            '\u{A000}',  // Yi Syllables, below Compatibility Ideographs
            '\u{D7B0}',  // Hangul Jamo Extended-B, just above Hangul syllables
            '\u{FB00}',  // Alphabetic Presentation Forms, above Compatibility
            '\u{FFF0}',  // just above the fullwidth block
            '\u{2FA20}', // just above Extensions B–F
            'a',
            'Z',
            '0',
            ' ',
            '\n',
            'é',
        ] {
            assert!(!is_cjk(c), "U+{:04X} must not be CJK", c as u32);
        }
    }

    /// ASCII and common Latin punctuation never classify as CJK.
    #[test]
    fn rejects_ascii() {
        for c in "hello, world![]{}()".chars() {
            assert!(!is_cjk(c), "ASCII {c:?} must not be CJK");
        }
    }
}
