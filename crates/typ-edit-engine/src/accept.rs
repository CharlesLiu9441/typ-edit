//! Pure generation-acceptance rules for worker results.
//!
//! A worker result is applied only when it is fresh for what it targets:
//!
//! - Tab-local latest-wins (parse, spell): the result must belong to the active
//!   tab and be at least as fresh as the applied generation.
//! - Workspace-global latest-wins (search, compile): freshness only, since the
//!   result has no per-tab identity.
//! - Exact-generation (IDE completions/references/rename): the result must match
//!   the caller-supplied current generation exactly — equal-or-nothing.

/// Latest-wins result for a specific tab (parse, spell): the result must belong
/// to the active tab and be at least as fresh as applied.
pub fn accepts_tab_latest(
    active_tab: u64,
    applied_generation: u64,
    result_tab: u64,
    result_generation: u64,
) -> bool {
    result_tab == active_tab && result_generation >= applied_generation
}

/// Latest-wins workspace-global result (search, compile): freshness only.
pub fn accepts_latest(applied_generation: u64, result_generation: u64) -> bool {
    result_generation >= applied_generation
}

/// Exact-generation result against the current document (ide completions/
/// references/rename): the caller passes the appropriate `current` (ide_result
/// generation for completions, doc generation for references/rename);
/// equal-or-nothing.
pub fn accepts_exact_current(current: Option<u64>, result_generation: u64) -> bool {
    current == Some(result_generation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_latest_accepts_matching_tab_at_equal_or_newer_generation() {
        assert!(accepts_tab_latest(7, 3, 7, 3), "equal generation is fresh");
        assert!(accepts_tab_latest(7, 3, 7, 4), "newer generation is fresh");
    }

    #[test]
    fn tab_latest_rejects_wrong_tab_or_stale_generation() {
        assert!(
            !accepts_tab_latest(7, 3, 8, 4),
            "a newer result for another tab is rejected"
        );
        assert!(
            !accepts_tab_latest(7, 3, 7, 2),
            "a stale result is rejected"
        );
    }

    #[test]
    fn latest_accepts_equal_or_newer_generation() {
        assert!(accepts_latest(5, 5), "equal generation is fresh");
        assert!(accepts_latest(5, 6), "newer generation is fresh");
        assert!(!accepts_latest(5, 4), "older generation is stale");
    }

    #[test]
    fn exact_current_requires_a_matching_present_generation() {
        assert!(accepts_exact_current(Some(9), 9), "exact match is accepted");
        assert!(
            !accepts_exact_current(Some(9), 8),
            "older result is rejected"
        );
        assert!(
            !accepts_exact_current(Some(9), 10),
            "newer result is rejected"
        );
        assert!(
            !accepts_exact_current(None, 9),
            "no current generation rejects everything"
        );
    }
}
