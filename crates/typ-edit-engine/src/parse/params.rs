//! Completion extraction: named parameters and value slots of library calls.

use super::complete::{first_ident, last_ident};
use typ_edit_model::{BytePos, CallParams, CompletionItem, ValueSlot};
use typst::syntax::{LinkedNode, Side, SyntaxKind};

/// The full dotted path of a field access built purely from identifiers
/// (`grid.cell` becomes `"grid.cell"`). Falls back to the bare field name when
/// the target is not itself an identifier chain (`f().cell` becomes `"cell"`).
fn field_access_path(node: &LinkedNode<'_>) -> Option<String> {
    match node.kind() {
        SyntaxKind::Ident => Some(node.leaf_text().to_string()),
        SyntaxKind::FieldAccess => {
            let field = node.children().next_back()?;
            let field_name =
                (field.kind() == SyntaxKind::Ident).then(|| field.leaf_text().to_string())?;
            match node
                .children()
                .next()
                .and_then(|target| field_access_path(&target))
            {
                Some(target_path) => Some(format!("{target_path}.{field_name}")),
                None => Some(field_name),
            }
        }
        _ => None,
    }
}

/// Strips a trailing `.where` / `.with` selector modifier from a dotted path.
///
/// Both are methods that build a selector from a function (`heading.where(...)`,
/// `figure.with(...)`); the completable parameters belong to the base function.
/// Returns `None` when the path has no such suffix.
fn strip_selector_method(path: &str) -> Option<&str> {
    let base = path
        .strip_suffix(".where")
        .or_else(|| path.strip_suffix(".with"))?;
    (!base.is_empty()).then_some(base)
}

/// The last child of `args` that is neither trivia nor a zero-width parser
/// placeholder. A missing value after `fill:` leaves a zero-width `Error`, which
/// must not hide the preceding `Named`.
fn last_significant_arg<'a>(args: &LinkedNode<'a>) -> Option<LinkedNode<'a>> {
    args.children()
        .filter(|child| !matches!(child.kind(), SyntaxKind::Space | SyntaxKind::Parbreak))
        .rfind(|child| child.range().start < child.range().end)
}

/// Whether an unclosed `Args` currently expects a fresh argument.
///
/// The trailing whitespace is a parse artifact of whatever the list ends in: a
/// `,` (or the opening `(`) leaves the next slot open, while a `Named` argument
/// awaiting its value (`#pagebreak(weak: `) puts the caret in that value, not in
/// a new parameter slot.
fn ends_in_open_slot(args: &LinkedNode<'_>) -> bool {
    match last_significant_arg(args) {
        None => true,
        Some(last) => matches!(
            last.kind(),
            SyntaxKind::Comma | SyntaxKind::LeftParen | SyntaxKind::Error
        ),
    }
}

/// Whether an unclosed `Args` ends in a `Named` argument's value region.
///
/// `#pagebreak(weak: ` leaves the whitespace after the colon as a sibling of the
/// call; the caret there belongs to `weak`'s value, which is a legal completion
/// slot. A positional value (`#figure(a `) does not qualify: its caret continues
/// that value and offers no fresh parameter list.
fn ends_in_named_value(args: &LinkedNode<'_>) -> bool {
    last_significant_arg(args).is_some_and(|last| last.kind() == SyntaxKind::Named)
}

/// The `Args` of the unclosed call directly before a whitespace run the caret
/// sits in.
///
/// `#figure(a, ` parses the trailing `Space` as a sibling of the `FuncCall`
/// rather than as a child of its `Args`, so a walk up from the caret leaf never
/// reaches the argument list. Looking back over the whitespace siblings recovers
/// the call. A completed call (`#figure(a) `) carries a `RightParen` and must
/// not offer parameters after its closing delimiter; a list waiting on a named
/// argument's value is not a fresh slot either.
fn trailing_call_args<'a>(node: &LinkedNode<'a>, cursor: usize) -> Option<LinkedNode<'a>> {
    if !matches!(node.kind(), SyntaxKind::Space | SyntaxKind::Parbreak)
        || !(node.range().start <= cursor && cursor <= node.range().end)
    {
        return None;
    }
    let mut sibling = node.prev_sibling();
    while let Some(previous) = sibling {
        let call = match previous.kind() {
            SyntaxKind::Space | SyntaxKind::Parbreak => {
                sibling = previous.prev_sibling();
                continue;
            }
            SyntaxKind::FuncCall | SyntaxKind::SetRule => previous,
            // A `show` selector wraps its call in a `ShowRule`; recover the
            // inner call so trailing whitespace after an unclosed selector
            // (`#show heading.where(level: `) still resolves.
            SyntaxKind::ShowRule => previous
                .children()
                .rfind(|child| child.kind() == SyntaxKind::FuncCall)?,
            _ => return None,
        };
        let args = call
            .children()
            .find(|child| child.kind() == SyntaxKind::Args)?;
        // A completed call (`#figure(a) `) carries a `RightParen` and must not
        // offer parameters after its closing delimiter.
        if args
            .children()
            .any(|child| child.kind() == SyntaxKind::RightParen)
        {
            return None;
        }
        // An open slot (a `,` or the opening `(`) expects a fresh argument; a
        // trailing `Named` awaiting its value is a legal caret position too. A
        // trailing positional value is neither.
        if ends_in_open_slot(&args) || ends_in_named_value(&args) {
            return Some(args);
        }
        return None;
    }
    None
}

/// The parameter name and the byte offset just past the colon of a `Named`
/// argument.
///
/// The parser materializes the name, colon and any value as children; the
/// offset past the colon is the left edge of the value region. A missing value
/// only changes whether a zero-width `Error` placeholder follows the colon, so
/// the value region is the same whether or not a value node exists.
fn named_value_param(named: &LinkedNode<'_>) -> Option<(String, usize)> {
    let mut name: Option<String> = None;
    let mut colon_end: Option<usize> = None;
    for child in named.children() {
        match child.kind() {
            SyntaxKind::Ident if name.is_none() => {
                name = Some(child.leaf_text().to_string());
            }
            SyntaxKind::Colon => colon_end = Some(child.range().end),
            _ => {}
        }
    }
    Some((name?, colon_end?))
}

/// The parameter whose value region contains the cursor inside `args`, together
/// with its `Named` node.
///
/// The caret belongs to a `Named` argument's value when it sits at or after its
/// colon and before the next argument's first non-trivia sibling (or the closing
/// delimiter). A value that already exists still resolves: the caller filters
/// the candidates by the typed prefix, so `fill: r|ed` completes `red`.
fn args_value_param<'a>(args: &LinkedNode<'a>, cursor: usize) -> Option<(String, LinkedNode<'a>)> {
    let children: Vec<LinkedNode<'a>> = args.children().collect();
    for (index, child) in children.iter().enumerate() {
        if child.kind() != SyntaxKind::Named {
            continue;
        }
        let Some((param, colon_end)) = named_value_param(child) else {
            continue;
        };
        if cursor < colon_end {
            continue;
        }
        let boundary = children[index + 1..]
            .iter()
            .find(|sibling| !matches!(sibling.kind(), SyntaxKind::Space | SyntaxKind::Parbreak))
            .map_or(args.range().end, |sibling| sibling.range().start);
        if cursor <= boundary {
            return Some((param, child.clone()));
        }
    }
    None
}

/// The digit-led numeric prefix of the value token ending exactly at the caret
/// (`12`, `12.5`), if any. Backs the unit candidates offered for a typed number.
fn numeric_value_prefix(leaf: &LinkedNode<'_>, cursor: usize) -> Option<String> {
    if leaf.range().end != cursor {
        return None;
    }
    let digits: String = leaf
        .leaf_text()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits
        .starts_with(|c: char| c.is_ascii_digit())
        .then_some(digits)
}

/// Resolves the named-parameter candidates at the cursor.
///
/// The cursor must sit in an argument slot of a call whose callee is a
/// standard-library function: right after the `(` or after a `,` the caret has
/// moved past, or anywhere in a named argument's value region (a fresh slot or a
/// partially typed value). A value-slot position yields value candidates when
/// the parameter accepts any; a string, comment or nested code/math/raw value is
/// no argument slot and yields `None`. Parameters already passed by name in the
/// same call are skipped. This mirrors `typst-ide`'s `complete_params`.
pub(crate) fn call_params(root: &LinkedNode<'_>, cursor: usize) -> Option<CallParams> {
    let leaf = root
        .leaf_at(cursor, Side::Before)
        .or_else(|| root.leaf_at(cursor, Side::After))?;

    // Walk up to the enclosing argument list. The cursor must sit in an open
    // slot: not in a string/comment, not in a code/math/raw value, not in a
    // callee or spread, and not on the closing delimiter or on a comma that the
    // caret has not moved past yet.
    let mut node = leaf.clone();
    let args = loop {
        // The trailing whitespace of an unclosed call sits outside its `Args`;
        // recover the argument list before the ancestor walk escapes it.
        if let Some(args) = trailing_call_args(&node, cursor) {
            break args;
        }
        match node.kind() {
            SyntaxKind::Str | SyntaxKind::LineComment | SyntaxKind::BlockComment => return None,
            // A code block, equation, math call or raw switches mode; the caret
            // there is no named argument's value region. Content blocks are
            // markup for the same reason and are rejected like before.
            SyntaxKind::ContentBlock
            | SyntaxKind::CodeBlock
            | SyntaxKind::Equation
            | SyntaxKind::Math
            | SyntaxKind::MathArgs
            | SyntaxKind::Raw => return None,
            SyntaxKind::FuncCall | SyntaxKind::Spread => return None,
            SyntaxKind::RightParen => return None,
            // A comma only closes the slot once the caret has moved past it.
            SyntaxKind::Comma => {
                if cursor < node.range().end {
                    return None;
                }
            }
            SyntaxKind::Args => break node,
            _ => {}
        }
        node = node.parent()?.clone();
    };

    // Only parenthesized argument lists take named parameters. An unclosed `(`
    // is reported as an `Error` leaf carrying the bracket text.
    let opens_with_paren = args.children().next().is_some_and(|first| {
        first.kind() == SyntaxKind::LeftParen
            || (first.kind().is_error() && first.leaf_text().as_str() == "(")
    });
    if !opens_with_paren {
        return None;
    }

    // The trailing whitespace of an unclosed call lives past `Args`, so a caret
    // there clamps onto the end of the last argument; the value region otherwise
    // ends before the whitespace and `cursor <= boundary` would reject it.
    let value_cursor = cursor.min(args.range().end);
    let value_param = args_value_param(&args, value_cursor);

    let call = args.parent()?;
    // A `set` rule stores its target and `Args` directly on the rule node —
    // there is no `FuncCall` for `set text(...)`. A `show` selector is a
    // normal call, including the `heading.where(...)` form.
    let callee = match call.kind() {
        SyntaxKind::FuncCall => call
            .children()
            .find(|child| child.kind() != SyntaxKind::Args)?,
        SyntaxKind::SetRule => call
            .children()
            .find(|child| matches!(child.kind(), SyntaxKind::Ident | SyntaxKind::FieldAccess))?,
        _ => return None,
    };
    let name = match callee.kind() {
        SyntaxKind::Ident => callee.leaf_text().to_string(),
        SyntaxKind::FieldAccess => {
            let path = field_access_path(&callee);
            // `.where(...)` / `.with(...)` derive a selector from another
            // function; complete that base function's parameters instead.
            path.as_deref()
                .and_then(strip_selector_method)
                .filter(|base| crate::completions::function_params(base).is_some())
                .map(str::to_owned)
                .or_else(|| path.filter(|path| crate::completions::function_params(path).is_some()))
                .unwrap_or(last_ident(&callee)?)
        }
        _ => return None,
    };
    let params = crate::completions::function_params(&name)?;

    let passed: Vec<String> = args
        .children()
        .filter(|child| child.kind() == SyntaxKind::Named)
        .filter_map(|child| first_ident(&child))
        .collect();
    let items: Vec<CompletionItem> = params
        .iter()
        .filter(|item| !passed.contains(&item.name))
        .cloned()
        .collect();

    // A number typed in a unit-accepting value slot completes with units.
    let unit_candidates = value_param.as_ref().and_then(|(param, _)| {
        let digits = numeric_value_prefix(&leaf, cursor)?;
        let units = crate::values::param_unit_suffixes(&name, param);
        (!units.is_empty()).then(|| crate::values::unit_items(&digits, &units))
    });

    // In a named argument's value position the caller only cares about value
    // candidates; when the parameter declares none, the slot stays unresolved.
    let value_slot = match value_param {
        Some((param, named)) => match unit_candidates {
            Some(items) => Some(ValueSlot { param, items }),
            None => {
                let candidate_items: Vec<CompletionItem> =
                    match super::dict::dict_group(&leaf, &named, cursor)
                        .and_then(|group| super::dict::dict_key_items(&param, &group))
                    {
                        Some(keys) => keys,
                        None => crate::values::param_value_items(&name, &param).to_vec(),
                    };
                if candidate_items.is_empty() {
                    return None;
                }
                Some(ValueSlot {
                    param,
                    items: candidate_items,
                })
            }
        },
        None => None,
    };

    (!items.is_empty() || value_slot.is_some()).then_some(CallParams {
        range: BytePos(args.range().start)..BytePos(args.range().end),
        items,
        value_slot,
    })
}
