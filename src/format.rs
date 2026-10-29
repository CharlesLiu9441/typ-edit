//! Typst source formatting via the `typstyle-core` pretty printer.

use typstyle_core::{Config, Typstyle};

/// Formats `source` with typstyle (2-space indent, otherwise default config).
///
/// Returns `None` when the source is empty or does not parse. Syntax errors are
/// a normal state while editing, so they are logged at debug level and never
/// surfaced as a user-facing error.
pub fn format_source(source: &str) -> Option<String> {
    if source.trim().is_empty() {
        return None;
    }
    let config = Config {
        tab_spaces: 2,
        ..Config::default()
    };
    match Typstyle::new(config).format_text(source).render() {
        Ok(formatted) => Some(formatted),
        Err(err) => {
            log::debug!("typstyle cannot format source: {err}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format_source;

    /// Already-formatted sample reused by the idempotence test.
    const SAMPLE: &str = "#let x = 1\n#let y = 2\n";

    #[test]
    fn formats_a_small_snippet() {
        let formatted = format_source("#let x=1").expect("snippet parses");
        assert!(
            formatted.contains("x = 1"),
            "operator spacing is normalised: {formatted:?}"
        );
    }

    #[test]
    fn output_is_lf_and_ends_with_newline() {
        let formatted = format_source("#let x = 1\r\n#let y = 2\r\n").expect("snippet parses");
        assert!(!formatted.contains('\r'), "output must be LF only");
        assert!(formatted.ends_with('\n'), "output must end with a newline");
    }

    #[test]
    fn syntax_error_returns_none() {
        assert!(
            format_source("#let x = (").is_none(),
            "an unclosed group must not format"
        );
    }

    #[test]
    fn format_source_skips_empty_input() {
        assert!(format_source("").is_none(), "empty input must not format");
        assert!(
            format_source("  \n\t\n").is_none(),
            "whitespace-only input must not format"
        );
    }

    #[test]
    fn formatting_is_idempotent() {
        let once = format_source(SAMPLE).expect("sample parses");
        let twice = format_source(&once).expect("formatted sample still parses");
        assert_eq!(once, twice);
    }
}
