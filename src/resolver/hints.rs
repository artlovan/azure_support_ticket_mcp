//! Hint extraction: pull a resource-name hint out of user-provided text.
//!
//! Two strictness levels live here, used by different callers:
//!
//! - [`first_quoted_identifier`] — strict, requires the hint to appear in
//!   `"..."`, `'...'`, or `` `...` `` quotes. Used by
//!   `preview_draft::collect_warnings` to decide whether to fire the strong
//!   "RESOURCE NOT RESOLVED" warning. False positives there are costly
//!   (would nag on every draft mentioning common words), so we demand
//!   high signal.
//! - [`extract_search_hint`] — broader, tries the quoted form first and
//!   falls back to whole-input and per-token identifier matching. Used by
//!   `resolve_context::maybe_search_resources` to decide whether to call
//!   Azure Resource Graph. False positives here are cheap (one RG query
//!   that returns nothing → "Other — paste ARM ID" sentinel), so we
//!   prefer to err on the side of trying.

/// Stopwords kept out of [`extract_search_hint`]'s per-token fallback —
/// common words that would otherwise be sent to Resource Graph as bogus
/// search hints. Kept short on purpose: the cost of a wasted RG query is
/// tiny; the cost of missing a real resource name is real frustration.
const PROSE_STOPWORDS: &[&str] = &[
    "azure",
    "support",
    "ticket",
    "issue",
    "issues",
    "error",
    "errors",
    "problem",
    "please",
    "cannot",
    "doesnt",
    "doesn",
    "wont",
    "open",
    "create",
    "delete",
    "update",
    "cluster",
    "scale",
    "deployment",
    "resource",
    "service",
    "account",
    "subscription",
    "tenant",
];

/// Pull the first quoted identifier from `s`. Recognizes `"..."`, `'...'`,
/// and `` `...` `` quoting styles. The content must look like an identifier
/// (alphanumeric, dashes, underscores, dots — typical of Azure resource
/// names), between 3 and 80 chars. Returns the content without quotes.
pub fn first_quoted_identifier(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let q = bytes[i];
        if q == b'"' || q == b'\'' || q == b'`' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != q {
                j += 1;
            }
            if j < bytes.len() && j > start {
                let inner = &s[start..j];
                let len = inner.chars().count();
                if (3..=80).contains(&len) && is_identifier_like(inner) {
                    return Some(inner.to_string());
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    None
}

/// Best-effort resource-name extraction for Resource Graph searches.
///
/// Tries three strategies in order, returning the first non-empty hint:
///
/// 1. **Quoted identifier** (highest signal) — same as
///    [`first_quoted_identifier`].
/// 2. **Whole-input identifier** (medium signal) — when the user (or the
///    calling model) provides just the bare name like `testtest123test`,
///    `prod-aks`, or `gpt-4o-prod`. This is the common case in practice
///    because models tend to strip quotes when summarizing user prompts
///    into structured tool calls.
/// 3. **First identifier-like token in prose** (lowest signal) — handles
///    "the account testtest123test won't scale" by picking the one token
///    that looks like a resource name. Stopwords (common English words)
///    are filtered out to avoid sending bogus queries for things like
///    `cluster` or `delete`.
///
/// Returns `None` only when no plausible identifier is found anywhere.
/// Callers should pass the returned hint through `sanitize_hint` (in
/// `resource_search`) before embedding in KQL.
pub fn extract_search_hint(s: &str) -> Option<String> {
    // 1. Quoted identifier — highest signal.
    if let Some(q) = first_quoted_identifier(s) {
        return Some(q);
    }
    // 2. Whole input is identifier-like — the common "user typed just the
    //    name" case, even when the model strips quotes before the tool call.
    let trimmed = s.trim();
    let trimmed_len = trimmed.chars().count();
    if (3..=80).contains(&trimmed_len) && is_identifier_like(trimmed) {
        return Some(trimmed.to_string());
    }
    // 3. Find one identifier-like token in prose. Skip stopwords + things
    //    too short to be a name. Return the longest qualifying token so
    //    "the prod-aks-cluster issue" prefers `prod-aks-cluster` over `the`.
    s.split_whitespace()
        .filter(|w| {
            let len = w.chars().count();
            (4..=80).contains(&len)
                && is_identifier_like(w)
                && !PROSE_STOPWORDS.iter().any(|sw| sw.eq_ignore_ascii_case(w))
        })
        .max_by_key(|w| w.chars().count())
        .map(String::from)
}

/// True when `s` consists entirely of ASCII alphanumeric or `-`, `_`, `.`
/// AND contains at least one alphanumeric character. Matches the character
/// set used by Azure resource names.
pub fn is_identifier_like(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && s.chars().any(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_three_quote_styles() {
        assert_eq!(
            first_quoted_identifier("Cannot delete \"contoso-b2c\""),
            Some("contoso-b2c".to_string())
        );
        assert_eq!(
            first_quoted_identifier("Cannot delete 'prod-aks' today"),
            Some("prod-aks".to_string())
        );
        assert_eq!(
            first_quoted_identifier("Cannot delete `gpt-4o-prod` deployment"),
            Some("gpt-4o-prod".to_string())
        );
    }

    #[test]
    fn skips_non_identifier_quotes() {
        // Punctuation/spaces inside quotes don't trigger.
        assert_eq!(
            first_quoted_identifier("Says \"hello world!\" but no resource"),
            None
        );
        // Empty quotes.
        assert_eq!(first_quoted_identifier("Empty: \"\""), None);
        // Too short to be a name.
        assert_eq!(first_quoted_identifier("Tag: \"x\""), None);
        // Unquoted tokens don't trigger.
        assert_eq!(first_quoted_identifier("Bug 12345 reported"), None);
    }

    #[test]
    fn is_identifier_like_basics() {
        assert!(is_identifier_like("contoso-b2c"));
        assert!(is_identifier_like("prod-aks"));
        assert!(is_identifier_like("gpt-4o_prod.v2"));
        assert!(!is_identifier_like("hello world"));
        assert!(!is_identifier_like(""));
        assert!(!is_identifier_like("---"), "must have at least one alnum");
        assert!(!is_identifier_like("a!b"));
    }

    // --- extract_search_hint: the three-strategy fallback -------------------
    //
    // Regression guard: testtest123test (and similar bare-name inputs)
    // must trigger a search. Before this extractor, only quoted identifiers
    // worked, which silently broke the common case where a model strips
    // quotes when summarizing user input into tool calls.

    #[test]
    fn extract_search_hint_finds_quoted_first() {
        assert_eq!(
            extract_search_hint("Cannot delete \"contoso-b2c\" resource"),
            Some("contoso-b2c".to_string())
        );
    }

    #[test]
    fn extract_search_hint_uses_whole_input_when_bare_identifier() {
        // The case that broke in real testing — user (or model) passed just
        // the name, no quotes.
        assert_eq!(
            extract_search_hint("testtest123test"),
            Some("testtest123test".to_string())
        );
        assert_eq!(
            extract_search_hint("prod-aks"),
            Some("prod-aks".to_string())
        );
        assert_eq!(
            extract_search_hint("  gpt-4o-prod  "),
            Some("gpt-4o-prod".to_string()),
            "should trim surrounding whitespace"
        );
    }

    #[test]
    fn extract_search_hint_picks_longest_identifier_in_prose() {
        // Embedded-in-prose case. `the` is too short, `account` is a
        // stopword; only `testtest123test` qualifies.
        assert_eq!(
            extract_search_hint("the account testtest123test"),
            Some("testtest123test".to_string())
        );
        // Prefer the longest qualifying token when several are present.
        assert_eq!(
            extract_search_hint("foo and prod-aks-cluster-east"),
            Some("prod-aks-cluster-east".to_string())
        );
    }

    #[test]
    fn extract_search_hint_skips_stopwords() {
        // Pure-stopword prose returns None — no point hitting RG with
        // queries for `delete`, `cluster`, etc.
        assert_eq!(extract_search_hint("please delete the cluster"), None);
        assert_eq!(extract_search_hint("Azure support ticket issue"), None);
    }

    #[test]
    fn extract_search_hint_returns_none_for_prose_without_identifiers() {
        // All-stopword prose returns None.
        assert_eq!(extract_search_hint("please open a ticket"), None);
        // Empty input.
        assert_eq!(extract_search_hint(""), None);
        // All tokens too short to qualify.
        assert_eq!(extract_search_hint("a b c"), None);
    }

    #[test]
    fn extract_search_hint_intentionally_trips_on_short_words_outside_stopwords() {
        // Documenting the design: bare prose with a plausible-looking word
        // returns that word. Cost of a false positive is one wasted RG
        // query that returns nothing → user sees the sentinel options.
        // Cost of NOT extracting would be missing real short resource names
        // (some Azure resources are named `auth`, `prod`, etc.).
        assert_eq!(
            extract_search_hint("my thing won't work"),
            Some("thing".to_string()),
            "deliberate: short non-stopword tokens are candidate hints"
        );
    }
}
