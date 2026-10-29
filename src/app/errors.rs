use super::ErrorRow;
use super::caret::{line_column_from_starts, line_starts};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use typ_edit_i18n::{t, t_with};
use typ_edit_model::Diag;
use typ_edit_preview::PreviewAction;
use typ_edit_widgets::list::error_row;
use typ_edit_widgets::theme::Theme;

/// Dialog error text for a failed create-file/create-folder operation.
pub(super) fn create_error_message(err: &std::io::Error, is_dir: bool) -> String {
    if err.kind() == std::io::ErrorKind::AlreadyExists {
        if is_dir {
            t("error.exists_folder").into_owned()
        } else {
            t("error.exists_file").into_owned()
        }
    } else {
        t_with("error.create_failed", &[("err", &err.to_string())])
    }
}

/// Rows for the preview error list: `file:line:column` + message.
///
/// Open-file rows ask `open_line_column` for the row and column, so the caller
/// can use the buffer's incremental line index without materializing the whole
/// document. External diagnostic files are read and indexed once per call.
pub(super) fn build_error_rows(
    diagnostics: &[Diag],
    open_path: Option<&Path>,
    open_line_column: impl Fn(usize) -> (usize, usize),
    root: Option<&Path>,
) -> Vec<ErrorRow> {
    let shown_path = |path: &Path| -> String {
        root.and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path)
            .display()
            .to_string()
    };

    let mut cache: HashMap<PathBuf, (String, Vec<usize>)> = HashMap::new();
    let mut rows = Vec::with_capacity(diagnostics.len());
    for diag in diagnostics {
        let location = if open_path == Some(diag.path.as_path()) {
            let (line, column) = open_line_column(diag.range.start.0);
            format!("{}:{}:{}", shown_path(&diag.path), line, column)
        } else {
            if !cache.contains_key(&diag.path) {
                let loaded = match crate::platform::fs_ops::read_to_string(&diag.path) {
                    Ok(text) => text,
                    Err(err) => {
                        log::warn!("cannot read {}: {err}", diag.path.display());
                        String::new()
                    }
                };
                let starts = line_starts(&loaded);
                cache.insert(diag.path.clone(), (loaded, starts));
            }
            match cache.get(&diag.path) {
                Some((text, starts)) => {
                    let (line, column) = line_column_from_starts(text, diag.range.start.0, starts);
                    format!("{}:{}:{}", shown_path(&diag.path), line, column)
                }
                None => format!("{}:{}", shown_path(&diag.path), diag.range.start.0),
            }
        };
        rows.push(ErrorRow {
            location,
            message: diag.message.clone(),
            path: diag.path.clone(),
            offset: diag.range.start.0,
            severity: diag.severity,
        });
    }
    rows
}

/// First error of a failed compile: the first error diagnostic, else the fatal error.
pub(super) fn first_error_message(rows: &[ErrorRow], compile_error: &Option<String>) -> String {
    rows.iter()
        .find(|row| row.severity == typ_edit_model::Severity::Error)
        .map(|row| row.message.clone())
        .or_else(|| compile_error.clone())
        .unwrap_or_else(|| t("error.compile_failed").into_owned())
}

/// Compact header of the preview's bottom error bar. When the canvas is still showing
/// the last successful preview, say so; otherwise report a plain compile failure.
pub(super) fn error_bar_header(showing_previous_preview: bool, error_count: usize) -> String {
    if showing_previous_preview {
        t_with(
            "error.compile_failed_prev",
            &[("count", &error_count.to_string())],
        )
    } else {
        t_with(
            "error.compile_failed_count",
            &[("count", &error_count.to_string())],
        )
    }
}

/// Clickable diagnostics of the preview's bottom error bar: the fatal compile error
/// A plain-language explanation for the most common Typst compiler errors.
///
/// The raw compiler message is still shown; this hint gives a non-programmer a
/// concrete thing to check. Unknown messages return `None`.
fn friendly_hint(message: &str) -> Option<String> {
    let lower = message.to_ascii_lowercase();
    let key = if lower.contains("unexpected end of block") {
        "error.hint.unexpected_end"
    } else if lower.contains("unknown variable") {
        "error.hint.unknown_variable"
    } else if lower.contains("unknown font") {
        "error.hint.unknown_font"
    } else if lower.contains("expected semicolon") {
        "error.hint.missing_semicolon"
    } else if lower.contains("expected closing") || lower.contains("unclosed") {
        "error.hint.expected_closing"
    } else if lower.contains("file not found") || lower.contains("failed to load") {
        "error.hint.file_not_found"
    } else {
        return None;
    };
    Some(t(key).into_owned())
}

/// (when the compile produced no diagnostics) above a scrolling list of error/warning
/// rows. Only shown by the pane while the latest compile actually has errors.
pub(super) fn error_list(
    ui: &mut egui::Ui,
    rows: &[ErrorRow],
    compile_error: Option<&str>,
) -> PreviewAction {
    use typ_edit_model::Severity;

    let theme = Theme::from_ui(ui);
    let mut action = PreviewAction::None;
    if let Some(error) = compile_error {
        ui.label(egui::RichText::new(error).color(theme.colors.error_text));
    }
    if rows.is_empty() {
        return action;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for row in rows {
                let color = match row.severity {
                    Severity::Error => theme.colors.error_text,
                    Severity::Warning => theme.colors.warn_text,
                };
                let response = error_row(
                    ui,
                    &format!("{}  {}", row.location, row.message),
                    friendly_hint(&row.message).as_deref(),
                    color,
                );
                if response.clicked() {
                    action = PreviewAction::JumpToSource {
                        path: row.path.clone(),
                        byte_offset: row.offset,
                    };
                }
            }
        });
    action
}

#[cfg(test)]
mod tests {
    use super::{build_error_rows, friendly_hint};
    use std::path::{Path, PathBuf};
    use typ_edit_model::{BytePos, Diag, Severity};

    fn diag(path: &str, start: usize) -> Diag {
        Diag {
            path: PathBuf::from(path),
            range: BytePos(start)..BytePos(start + 1),
            message: "bad".to_owned(),
            severity: Severity::Error,
        }
    }

    #[test]
    fn open_file_rows_use_the_supplied_line_index() {
        let rows = build_error_rows(
            &[diag("main.typ", 5)],
            Some(Path::new("main.typ")),
            |byte| {
                assert_eq!(byte, 5);
                (3, 4)
            },
            None,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].location, "main.typ:3:4");
    }

    #[test]
    fn no_diagnostics_never_calls_the_line_index() {
        let rows = build_error_rows(
            &[],
            Some(Path::new("main.typ")),
            |_: usize| -> (usize, usize) { panic!("line index must not be called") },
            None,
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn friendly_hint_maps_common_compiler_errors() {
        assert!(friendly_hint("unexpected end of block").is_some());
        assert!(friendly_hint("unknown variable: result").is_some());
        assert!(friendly_hint("unknown font family: Foo").is_some());
        assert!(friendly_hint("expected semicolon").is_some());
        assert!(friendly_hint("unclosed delimiter").is_some());
        assert!(friendly_hint("file not found (path)").is_some());
        assert!(friendly_hint("something completely different").is_none());
    }
}
