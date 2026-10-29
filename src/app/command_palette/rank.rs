//! Pure fuzzy filtering and selection helpers for the command palette.

use crate::commands::CommandId;
use typ_edit_i18n::t;

/// Base score for every matched query character.
const MATCH_BASE: i32 = 1;
/// Bonus for a query character matched right after the previous one.
const CONSECUTIVE_BONUS: i32 = 30;
/// Bonus for a query character matched at a word boundary.
const WORD_START_BONUS: i32 = 10;
/// Bonuses that keep a label match ahead of an id-only or category-only match.
const LABEL_BONUS: i32 = 1_000;
const ID_BONUS: i32 = 500;
const CATEGORY_BONUS: i32 = 250;

/// Ranks the registry items for `query`, best match first.
///
/// An empty query keeps registry order. Matching is a case-insensitive
/// subsequence over the translated label first, then the command id string,
/// then the translated category; a label match always outranks an id-only or
/// category-only match. Ties keep registry order.
pub(super) fn rank_commands(query: &str, items: &[(CommandId, String)]) -> Vec<CommandId> {
    let query = query.trim();
    if query.is_empty() {
        return items.iter().map(|(id, _)| *id).collect();
    }
    let mut scored: Vec<(i32, usize, CommandId)> = Vec::new();
    for (index, (id, label)) in items.iter().enumerate() {
        let score = if let Some(score) = fuzzy_score(query, label) {
            Some(score + LABEL_BONUS)
        } else if let Some(score) = fuzzy_score(query, &id.as_str().replace('_', " ")) {
            Some(score + ID_BONUS)
        } else {
            id.meta()
                .category
                .and_then(|key| fuzzy_score(query, &t(key)))
                .map(|score| score + CATEGORY_BONUS)
        };
        if let Some(score) = score {
            scored.push((score, index, *id));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, id)| id).collect()
}

/// Scores a case-insensitive subsequence match of `needle` in `haystack`.
///
/// Returns `None` when `needle` is not a subsequence. Each matched character
/// scores [`MATCH_BASE`]; a run adds [`CONSECUTIVE_BONUS`], a word-boundary
/// match adds [`WORD_START_BONUS`], and the first match subtracts its index so
/// an earlier match wins.
fn fuzzy_score(needle: &str, haystack: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let needle: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    let hay: Vec<char> = haystack.chars().flat_map(char::to_lowercase).collect();
    let mut score = 0i32;
    let mut cursor = 0usize;
    let mut previous: Option<usize> = None;
    for want in needle {
        let mut found = None;
        while cursor < hay.len() {
            if hay[cursor] == want {
                found = Some(cursor);
                break;
            }
            cursor += 1;
        }
        let index = found?;
        score += MATCH_BASE;
        if previous == Some(index.saturating_sub(1)) {
            score += CONSECUTIVE_BONUS;
        }
        if index == 0 || !hay[index - 1].is_alphanumeric() {
            score += WORD_START_BONUS;
        }
        if previous.is_none() {
            score -= index as i32;
        }
        previous = Some(index);
        cursor += 1;
    }
    Some(score)
}

/// Moves `selected` by `delta`, skipping disabled rows; clamps at either end.
pub(super) fn move_selection(enabled: &[bool], selected: usize, delta: isize) -> usize {
    let len = enabled.len();
    if len == 0 || delta == 0 {
        return selected;
    }
    let mut index = selected as isize;
    loop {
        index += delta;
        if index < 0 || index >= len as isize {
            return selected;
        }
        if enabled[index as usize] {
            return index as usize;
        }
    }
}

/// Clamps `selected` into range and snaps it to the nearest enabled row
/// (searching forward first, then backward) when it lands on a disabled one.
pub(super) fn clamp_selection(enabled: &[bool], selected: usize) -> usize {
    if enabled.is_empty() {
        return 0;
    }
    let selected = selected.min(enabled.len() - 1);
    if enabled[selected] {
        return selected;
    }
    if let Some(index) = (selected..enabled.len()).find(|&index| enabled[index]) {
        return index;
    }
    if let Some(index) = (0..selected).rev().find(|&index| enabled[index]) {
        return index;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_score_matches_subsequences_case_insensitively() {
        assert!(fuzzy_score("cp", "Command Palette").is_some());
        assert!(fuzzy_score("CMP", "Command Palette").is_some());
        assert!(fuzzy_score("", "anything").is_some());
        assert!(fuzzy_score("zzz", "Command Palette").is_none());
        assert!(fuzzy_score("palette command", "Command Palette").is_none());
    }

    #[test]
    fn rank_prefers_prefix_and_consecutive_matches() {
        let items = vec![
            (CommandId::SelectAll, "Select All".to_owned()),
            (CommandId::Save, "Save All".to_owned()),
        ];
        assert_eq!(
            rank_commands("sa", &items),
            vec![CommandId::Save, CommandId::SelectAll]
        );
    }

    #[test]
    fn empty_query_preserves_registry_order() {
        let items = vec![
            (CommandId::ShowCommandPalette, "Command Palette".to_owned()),
            (CommandId::Save, "Save".to_owned()),
            (CommandId::OpenSettings, "Settings".to_owned()),
        ];
        let expected = vec![
            CommandId::ShowCommandPalette,
            CommandId::Save,
            CommandId::OpenSettings,
        ];
        assert_eq!(rank_commands("", &items), expected);
        assert_eq!(rank_commands("   ", &items), expected);
    }

    #[test]
    fn rank_matches_the_command_id_when_the_label_does_not() {
        let items = vec![(CommandId::OpenSettings, "Settings".to_owned())];
        assert_eq!(rank_commands("open", &items), vec![CommandId::OpenSettings]);
    }

    #[test]
    fn move_selection_skips_disabled_rows_and_clamps() {
        let enabled = [true, false, true, true];
        assert_eq!(move_selection(&enabled, 0, 1), 2);
        assert_eq!(move_selection(&enabled, 2, -1), 0);
        assert_eq!(move_selection(&enabled, 3, 1), 3);
        assert_eq!(move_selection(&enabled, 0, -1), 0);
    }

    #[test]
    fn clamp_selection_snaps_to_the_nearest_enabled_row() {
        assert_eq!(clamp_selection(&[true, false, false, true], 1), 3);
        assert_eq!(clamp_selection(&[true, false, false, true], 4), 3);
        assert_eq!(clamp_selection(&[false, true], 0), 1);
        assert_eq!(clamp_selection(&[], 5), 0);
    }
}
