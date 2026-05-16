//! Fuzzy entity-name normalization for the `remember` write path.
//!
//! When `ensure_entity` fails to find an exact `(name, entity_type)` match,
//! the normalizer scores the incoming name against recent entities of the
//! same type. Scoring uses two fast-path equality checks (stripped and
//! token-sorted forms) plus a conservative slow path that requires both
//! Jaro-Winkler and normalized Levenshtein to agree, preventing
//! prefix-biased false positives on short names like "Topic1" / "Topic10".
//!
//! Inputs without any alphanumeric characters (emoji-only, punctuation-only,
//! whitespace-only) bypass the alnum-stripping fast paths and fall back to
//! comparing the lowercased originals; otherwise every such pair would
//! collapse to a spurious `1.0` via empty-string equality.

use strsim::{jaro_winkler, normalized_levenshtein};

/// Outcome of fuzzy matching against existing entities. Only produced when
/// the exact-match path in `ensure_entity` misses; the no-match case is
/// represented by `Option::None` rather than a third variant.
#[derive(Debug, Clone, PartialEq)]
pub enum NormalizeMatch {
    /// The incoming name is similar enough to an existing entity that we
    /// silently redirect the write to that entity.
    AutoMerge { entity_id: String, score: f64 },
    /// The incoming name is plausibly the same entity but below the
    /// auto-merge bar; create a fresh entity and link it via `SAME_AS` so
    /// an operator can review.
    Flag { entity_id: String, score: f64 },
}

fn token_sort(s: &str) -> String {
    let mut words: Vec<&str> = s.split_whitespace().collect();
    words.sort_unstable();
    words.join(" ")
}

fn alnum_only(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// Score two entity names in `[0.0, 1.0]`. The score is symmetric, finite,
/// and lowercases both sides via `str::to_lowercase` before comparing. Note
/// that this is the *simple* Unicode lowercase mapping, not full case
/// folding: `ß` stays as `ß` rather than folding to `ss`, so `"Straße"` and
/// `"strasse"` do not match via the fast path. The golden-table test pins
/// the full behavioral contract.
#[must_use]
pub fn similarity(a: &str, b: &str) -> f64 {
    let a_low = a.to_lowercase();
    let b_low = b.to_lowercase();
    let a_stripped = alnum_only(&a_low);
    let b_stripped = alnum_only(&b_low);

    // Names with no alphanumeric content (pure emoji, punctuation, or
    // whitespace) carry no signal for the stripping-based fast paths and
    // would otherwise collapse to a universal 1.0 via empty-string
    // equality. Compare the lowercased originals instead. We deliberately
    // skip this branch when *both* sides have alnum content so the common
    // path stays a single pointer compare on `a_stripped`.
    if a_stripped.is_empty() || b_stripped.is_empty() {
        return raw_similarity(&a_low, &b_low);
    }

    // Fast path: identical after stripping punctuation and whitespace.
    if a_stripped == b_stripped {
        return 1.0;
    }

    // Fast path: identical after word reordering.
    let a_sorted = token_sort(&a_low);
    let b_sorted = token_sort(&b_low);
    if a_sorted == b_sorted {
        return 1.0;
    }

    // Slow path: JW and NLev must agree. JW is prefix-weighted and would
    // over-score "Topic1" / "Topic10"; NLev is proportional to edit
    // distance and corrects for that. Taking the maximum across the
    // token-sorted and stripped JW variants accommodates word reordering
    // and punctuation drift respectively, then we floor it with NLev.
    let jw = jaro_winkler(&a_sorted, &b_sorted).max(jaro_winkler(&a_stripped, &b_stripped));
    let nlev = normalized_levenshtein(&a_stripped, &b_stripped);
    jw.min(nlev)
}

/// Score path for inputs that have no alphanumeric characters to strip.
/// Mirrors the JW.min(NLev) rule from `similarity`. `strsim` already returns
/// `1.0` for equal inputs (including both-empty) and `0.0` when exactly one
/// side is empty, so no explicit guards are needed.
fn raw_similarity(a_low: &str, b_low: &str) -> f64 {
    jaro_winkler(a_low, b_low).min(normalized_levenshtein(a_low, b_low))
}

/// Pick the highest-scoring candidate and bucket it by the configured
/// thresholds. Returns `None` if no candidate beats both the implicit
/// floor (score must be strictly positive) and the configured flag
/// threshold. On a tie, the first candidate in iteration order wins;
/// callers depend on `updated_at DESC` ordering at the SQL layer to make
/// "first" mean "most recent".
#[must_use]
pub fn find_best_match(
    name: &str,
    candidates: &[(String, String)],
    auto_merge_threshold: f64,
    flag_threshold: f64,
) -> Option<NormalizeMatch> {
    let mut best_id: Option<&String> = None;
    let mut best_score = 0.0_f64;

    for (id, candidate_name) in candidates {
        let score = similarity(name, candidate_name);
        if score > best_score {
            best_score = score;
            best_id = Some(id);
        }
    }

    let id = best_id?;
    if best_score >= auto_merge_threshold {
        Some(NormalizeMatch::AutoMerge {
            entity_id: id.clone(),
            score: best_score,
        })
    } else if best_score >= flag_threshold {
        Some(NormalizeMatch::Flag {
            entity_id: id.clone(),
            score: best_score,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Default thresholds, matching `NormalizationSection::default_*`. The
    // contract pinned by the golden table is independent of any specific
    // `Config` value; we hard-code them here so a future config tweak
    // doesn't silently rewrite the score contract.
    const AUTO_MERGE: f64 = 0.95;
    const FLAG: f64 = 0.85;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Action {
        AutoMerge,
        Flag,
        NoMatch,
    }

    /// One row of the score-contract golden table.
    ///
    /// `min` and `max` bound the expected `similarity(a, b)` output (both
    /// inclusive). `action` is what `find_best_match` is expected to pick
    /// at the default thresholds; it must be consistent with the range,
    /// but is checked independently so a buggy range and a buggy action
    /// don't mask each other. `desc` shows up in failure messages so a
    /// regression points at the failing case by name.
    #[derive(Debug, Clone, Copy)]
    struct Case {
        a: &'static str,
        b: &'static str,
        min: f64,
        max: f64,
        action: Action,
        desc: &'static str,
    }

    impl Case {
        const fn merge(
            a: &'static str,
            b: &'static str,
            min: f64,
            max: f64,
            desc: &'static str,
        ) -> Self {
            Self {
                a,
                b,
                min,
                max,
                action: Action::AutoMerge,
                desc,
            }
        }

        const fn flag(
            a: &'static str,
            b: &'static str,
            min: f64,
            max: f64,
            desc: &'static str,
        ) -> Self {
            Self {
                a,
                b,
                min,
                max,
                action: Action::Flag,
                desc,
            }
        }

        const fn none(
            a: &'static str,
            b: &'static str,
            min: f64,
            max: f64,
            desc: &'static str,
        ) -> Self {
            Self {
                a,
                b,
                min,
                max,
                action: Action::NoMatch,
                desc,
            }
        }
    }

    fn action_for(score: f64) -> Action {
        if score >= AUTO_MERGE {
            Action::AutoMerge
        } else if score >= FLAG {
            Action::Flag
        } else {
            Action::NoMatch
        }
    }

    /// The full score-contract table. Adding rows here is the right way
    /// to capture a new normalization behavior; tightening a range is the
    /// right way to lock in a scoring improvement.
    #[rustfmt::skip]
    const CASES: &[Case] = &[
        // ---- Identity ----
        Case::merge("Raymond",       "Raymond",       1.0, 1.0, "identity"),
        Case::merge("openmemory",    "openmemory",    1.0, 1.0, "lowercase identity"),

        // ---- Case-only variation ----
        Case::merge("Raymond",       "raymond",       1.0, 1.0, "lower vs title"),
        Case::merge("OPENMEMORY",    "OpenMemory",    1.0, 1.0, "all-caps vs PascalCase"),
        Case::merge("ProjectAlpha",  "projectalpha",  1.0, 1.0, "PascalCase vs flat lower"),

        // ---- Concatenation / spacing ----
        Case::merge("ProjectAlpha",  "Project Alpha", 1.0, 1.0, "PascalCase vs spaced"),
        Case::merge("OpenMemory",    "open memory",   1.0, 1.0, "PascalCase vs lower spaced"),
        Case::merge("VS Code",       "vscode",        1.0, 1.0, "spaced acronym vs concat"),
        Case::merge("New York City", "newyorkcity",   1.0, 1.0, "three-word concat"),

        // ---- Word order ----
        Case::merge("Project Alpha", "Alpha Project", 1.0, 1.0, "two-word reorder"),
        Case::merge("Raymond Jow",   "Jow Raymond",   1.0, 1.0, "given/family reorder"),

        // ---- Punctuation drift ----
        Case::merge("open-memory",   "OpenMemory",    1.0, 1.0, "hyphen vs PascalCase"),
        Case::merge("open-memory",   "open memory",   1.0, 1.0, "hyphen vs space"),
        Case::merge("React.js",      "reactjs",       1.0, 1.0, "dotted module name"),
        Case::merge("claude_code",   "Claude Code",   1.0, 1.0, "underscore vs space"),
        Case::merge("PostgreSQL!",   "PostgreSQL",    1.0, 1.0, "trailing punctuation"),
        Case::merge("Project (Alpha)", "Project Alpha", 1.0, 1.0, "parentheses"),

        // ---- Whitespace handling ----
        Case::merge("Project  Alpha", "Project Alpha", 1.0, 1.0, "double space"),
        Case::merge(" Project Alpha ", "Project Alpha", 1.0, 1.0, "leading/trailing space"),
        Case::merge("Project\tAlpha", "Project Alpha", 1.0, 1.0, "tab vs space"),

        // ---- Unicode case mapping ----
        // `str::to_lowercase` applies the simple Unicode lowercase mapping,
        // not full case folding. "STRASSE".to_lowercase() == "strasse"
        // (multi-char uppercase sharp-s to ss already happened upstream), so this
        // pair merges. The next case pins the inverse: "Straße" lowercases to "straße"
        // under the same mapping, so it does *not* match "strasse" without
        // an explicit case-folding pass we deliberately don't perform.
        Case::merge("STRASSE",       "strasse",       1.0, 1.0, "uppercase ß expansion"),
        Case::none("Straße",         "strasse",       0.50, 0.85, "ß not folded to ss"),

        // ---- Common-prefix near-duplicates: MUST NOT auto-merge ----
        // JW alone would score these >= 0.95 (prefix bias); the NLev floor
        // is what keeps Topic1/Topic10 out of the merge bucket. Service A
        // / Service B is the borderline case: stripped forms differ by
        // exactly one character, so NLev = 7/8 = 0.875; above the flag
        // threshold but below auto-merge. Locking in current behavior; an
        // operator review surface is the right place to disambiguate.
        Case::flag("Topic1",         "Topic10",       0.85, 0.95, "prefix-extended index"),
        Case::flag("Service A",      "Service B",     0.85, 0.95, "trailing-letter discriminant"),

        // ---- Plural / suffix variants ----
        Case::flag("config",         "configs",       0.85, 0.95, "singular vs plural"),

        // ---- Near-miss / single-edit short names ----
        // sift vs swift: 1 insertion / 5 chars means NLev 0.8; below flag.
        // PostgreSQL vs Postgres: 2-char truncation on an 10-char stripped
        // form means NLev 0.8. Same band as sift/swift. The NLev floor that
        // protects against Topic1/Topic10 false positives also keeps this
        // pair from flagging; surfacing it would require an explicit alias
        // table or a more generous flag threshold.
        Case::none("sift",           "swift",         0.70, 0.85, "single-char insertion"),
        Case::none("PostgreSQL",     "Postgres",      0.70, 0.85, "suffix truncation"),
        Case::none("form",           "from",          0.40, 0.85, "adjacent transposition"),
        Case::none("color",          "colour",        0.70, 0.85, "British spelling variant"),

        // ---- Numeric suffix discriminants ----
        Case::none("user1",          "user2",         0.0,  0.85, "incrementing id"),
        Case::none("v1",             "v2",            0.0,  0.85, "version increment"),

        // ---- Substring / partial overlap ----
        Case::none("Project",        "Project Alpha", 0.0,  0.85, "prefix is full word"),
        Case::none("openmemory",     "open-memory-graph", 0.0, 0.85, "root vs extended name"),

        // ---- Unrelated ----
        Case::none("Raymond",        "Kubernetes",    0.0, 0.50, "unrelated"),
        Case::none("foo",            "bar",           0.0, 0.50, "unrelated short"),
        Case::none("ML Pipeline",    "Machine Learning Pipeline", 0.0, 0.85, "abbreviation expansion"),
        Case::none("javascript",     "java",          0.0, 0.85, "language families"),

        // ---- Unicode (non-folding) ----
        // Accented characters survive lowercasing intact; one-edit distance
        // is not enough to flag, matching the existing test expectation.
        Case::none("café",           "cafe",          0.70, 0.85, "accent vs ascii (one edit)"),
        Case::none("résumé",         "resume",        0.50, 0.85, "two diacritics"),
    ];

    #[test]
    fn golden_table_score_contract() {
        let mut failures: Vec<String> = Vec::new();
        for case in CASES {
            let score = similarity(case.a, case.b);
            let score_ok = (case.min..=case.max).contains(&score);
            let actual_action = action_for(score);
            let action_ok = actual_action == case.action;

            // The action and the score range must be consistent. We check
            // them independently so that, on a failure, the message says
            // *which* part of the contract drifted.
            if !score_ok || !action_ok {
                failures.push(format!(
                    "  [{desc}] similarity({a:?}, {b:?}) = {score:.4}; \
                     expected score in [{min}, {max}] ({score_status}), \
                     expected action {expected:?} got {actual:?} ({action_status})",
                    desc = case.desc,
                    a = case.a,
                    b = case.b,
                    score = score,
                    min = case.min,
                    max = case.max,
                    score_status = if score_ok { "OK" } else { "FAIL" },
                    expected = case.action,
                    actual = actual_action,
                    action_status = if action_ok { "OK" } else { "FAIL" },
                ));
            }
        }

        assert!(
            failures.is_empty(),
            "{count} of {total} golden cases failed:\n{detail}",
            count = failures.len(),
            total = CASES.len(),
            detail = failures.join("\n"),
        );
    }

    /// Belt-and-suspenders coverage for the regression that motivated this
    /// suite: two names whose alphanumeric-only forms are both empty must
    /// not collapse to similarity 1.0 (which would have auto-merged any
    /// two emoji-only or punctuation-only entity names).
    #[test]
    fn distinct_no_alphanumeric_names_do_not_collapse() {
        let rocket_vs_party = similarity("🚀", "🎉");
        assert!(
            rocket_vs_party < 0.5,
            "distinct emoji collapsed to {rocket_vs_party}; \
             the alnum-stripping fast path must not fire on empty strips"
        );

        let dashes_vs_pluses = similarity("---", "+++");
        assert!(
            dashes_vs_pluses < 0.5,
            "distinct punctuation strings collapsed to {dashes_vs_pluses}"
        );

        let emoji_vs_word = similarity("🚀", "Rocket");
        assert!(
            emoji_vs_word < 0.5,
            "emoji vs unrelated word collapsed to {emoji_vs_word}"
        );
    }

    /// A name with no alphanumeric content is still self-similar to itself
    /// because the fallback path preserves the `similarity(a, a) == 1.0` invariant.
    #[test]
    fn identical_no_alphanumeric_names_remain_self_similar() {
        assert!((similarity("🚀", "🚀") - 1.0).abs() < f64::EPSILON);
        assert!((similarity("---", "---") - 1.0).abs() < f64::EPSILON);
        assert!((similarity("   ", "   ") - 1.0).abs() < f64::EPSILON);
    }

    /// Whitespace-only names strip to "" but fall back to a raw compare.
    /// `MemoryStore::remember` rejects whitespace-only names upstream, so
    /// this test pins the library-level behavior, not a user-facing path.
    #[test]
    fn whitespace_only_inputs_are_self_similar() {
        assert!((similarity("", "") - 1.0).abs() < f64::EPSILON);
        assert!((similarity("\t", "\t") - 1.0).abs() < f64::EPSILON);
    }

    /// Empty paired with a real string scores 0, matching strsim's
    /// `jaro_winkler` / `normalized_levenshtein` convention.
    #[test]
    fn empty_vs_nonempty_scores_zero() {
        assert!(similarity("", "anything").abs() < f64::EPSILON);
        assert!(similarity("anything", "").abs() < f64::EPSILON);
    }

    /// Long, identical strings hit the fast path and return 1.0 without
    /// invoking the O(n²) Levenshtein DP. The 16 KiB length is well past
    /// any plausible entity name and confirms there's no hidden quadratic
    /// blow-up on the common-case path.
    #[test]
    fn long_identical_strings_match_via_fast_path() {
        let a = "a".repeat(16 * 1024);
        let score = similarity(&a, &a);
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    // ---- find_best_match ----

    #[test]
    fn find_best_match_returns_none_on_empty_candidates() {
        let result = find_best_match("anything", &[], AUTO_MERGE, FLAG);
        assert!(result.is_none());
    }

    #[test]
    fn find_best_match_auto_merges_at_or_above_threshold() {
        // similarity("Project Alpha", "project alpha") == 1.0 via the
        // stripped fast path. With auto_merge_threshold == 1.0, the
        // boundary check is `1.0 >= 1.0`, producing AutoMerge.
        let candidates = [("id".to_string(), "project alpha".to_string())];
        let result = find_best_match("Project Alpha", &candidates, 1.0, FLAG);
        match result {
            Some(NormalizeMatch::AutoMerge { entity_id, score }) => {
                assert_eq!(entity_id, "id");
                assert!((score - 1.0).abs() < f64::EPSILON);
            }
            other => panic!("expected AutoMerge at score==threshold, got {other:?}"),
        }
    }

    #[test]
    fn find_best_match_flags_at_or_above_threshold() {
        // configs vs config scores ~0.857; setting the flag threshold to
        // 0.85 should still produce Flag (inclusive `>=`).
        let candidates = [("id".to_string(), "config".to_string())];
        let result = find_best_match("configs", &candidates, 0.95, 0.85);
        assert!(
            matches!(result, Some(NormalizeMatch::Flag { ref entity_id, .. }) if entity_id == "id"),
            "expected Flag, got {result:?}"
        );
    }

    #[test]
    fn find_best_match_returns_none_below_flag_threshold() {
        let candidates = [("id".to_string(), "config".to_string())];
        let result = find_best_match("configs", &candidates, 0.99, 0.99);
        assert!(
            result.is_none(),
            "score below 0.99 must not match, got {result:?}"
        );
    }

    #[test]
    fn find_best_match_picks_highest_scoring_candidate() {
        let candidates = [
            ("low".to_string(), "totally unrelated".to_string()),
            ("high".to_string(), "project alpha".to_string()),
            ("mid".to_string(), "alpha".to_string()),
        ];
        let result = find_best_match("Project Alpha", &candidates, AUTO_MERGE, FLAG);
        match result {
            Some(NormalizeMatch::AutoMerge { entity_id, .. }) => {
                assert_eq!(entity_id, "high", "highest-scoring candidate must win");
            }
            other => panic!("expected AutoMerge on 'high', got {other:?}"),
        }
    }

    /// On a perfect-score tie, the first candidate in iteration order wins
    /// (the comparison uses strict `>`). The call site in `ensure_entity`
    /// passes candidates in `updated_at DESC`, so this rule means "most
    /// recently updated entity wins". Lock that in here.
    #[test]
    fn find_best_match_first_candidate_wins_on_tie() {
        let candidates = [
            ("first".to_string(), "project alpha".to_string()),
            ("second".to_string(), "project alpha".to_string()),
        ];
        let result = find_best_match("Project Alpha", &candidates, AUTO_MERGE, FLAG);
        match result {
            Some(NormalizeMatch::AutoMerge { entity_id, .. }) => assert_eq!(entity_id, "first"),
            other => panic!("expected first candidate to win, got {other:?}"),
        }
    }

    /// All-zero scoring candidates return None: there's no signal to pick
    /// a winner from, and an arbitrary "first one" pick would be a worse
    /// contract than a clean miss.
    #[test]
    fn find_best_match_returns_none_when_all_candidates_score_zero() {
        let candidates = [
            ("a".to_string(), "🚀".to_string()),
            ("b".to_string(), "🎉".to_string()),
        ];
        // Query has alnum content; candidates don't. similarity is 0 for both.
        let result = find_best_match("Rocket", &candidates, AUTO_MERGE, FLAG);
        assert!(
            result.is_none(),
            "all-zero candidates must miss, got {result:?}"
        );
    }

    // ---- Property tests ----

    mod properties {
        use super::*;
        use proptest::prelude::*;

        /// Plausible entity-name shapes. Kept in ASCII so case-folding
        /// invariants are clean because non-ASCII case-folding can change a
        /// string's length (for example, sharp-s to `SS` to `ss`), which would break
        /// the ASCII-style "uppercase then lowercase round-trip is a
        /// no-op" property the case-invariance test relies on.
        fn ascii_name() -> impl Strategy<Value = String> {
            "[a-zA-Z0-9 ._\\-]{1,32}"
        }

        /// Anything goes: arbitrary Unicode, possibly empty. Used by the
        /// panic-resistance test only; the metric invariants don't all
        /// apply to fully arbitrary inputs.
        fn any_string() -> impl Strategy<Value = String> {
            proptest::string::string_regex(".{0,32}").unwrap()
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            /// `similarity` must stay in `[0.0, 1.0]` and be finite for
            /// any pair of inputs. A NaN or out-of-range value would
            /// break threshold comparisons in `find_best_match`.
            #[test]
            fn score_is_finite_and_in_unit_interval(
                a in any_string(),
                b in any_string(),
            ) {
                let s = similarity(&a, &b);
                prop_assert!(s.is_finite(), "non-finite score {s} for ({a:?}, {b:?})");
                prop_assert!(
                    (0.0..=1.0).contains(&s),
                    "score {s} out of [0, 1] for ({a:?}, {b:?})",
                );
            }

            /// Order-independence: `similarity(a, b) == similarity(b, a)`.
            /// Threshold logic in `find_best_match` would be position-
            /// dependent if this didn't hold.
            #[test]
            fn similarity_is_symmetric(a in ascii_name(), b in ascii_name()) {
                let ab = similarity(&a, &b);
                let ba = similarity(&b, &a);
                prop_assert!(
                    (ab - ba).abs() < 1e-9,
                    "asymmetric: similarity({a:?}, {b:?}) = {ab} but \
                     similarity({b:?}, {a:?}) = {ba}",
                );
            }

            /// `similarity(a, a) == 1.0` for any non-empty input. The
            /// `ascii_name` strategy excludes the empty string so the
            /// trivially-vacuous case never fires here.
            #[test]
            fn self_similarity_is_one(a in ascii_name()) {
                let s = similarity(&a, &a);
                prop_assert!(
                    (s - 1.0).abs() < 1e-9,
                    "self-similarity {s} != 1.0 for {a:?}",
                );
            }

            /// ASCII case-shifting one side does not change the score;
            /// `similarity` lowercases both inputs internally.
            #[test]
            fn ascii_case_invariant(a in ascii_name(), b in ascii_name()) {
                let baseline = similarity(&a, &b);
                let shifted = similarity(&a.to_uppercase(), &b);
                prop_assert!(
                    (baseline - shifted).abs() < 1e-9,
                    "case shift moved score: {baseline} vs {shifted} \
                     for ({a:?}, {b:?})",
                );
            }

            /// Arbitrary Unicode (including 4-byte characters, control
            /// codes, and the empty string) must not panic. Outcome is
            /// irrelevant; the test fails iff `similarity` panics.
            #[test]
            fn does_not_panic_on_arbitrary_unicode(
                a in any_string(),
                b in any_string(),
            ) {
                let _ = similarity(&a, &b);
            }

            /// `find_best_match` bucketing is consistent with the
            /// underlying score. We construct a single-candidate set so
            /// the relationship is one-to-one and verify all three
            /// outcomes against the corresponding score band.
            #[test]
            fn find_best_match_buckets_match_score_bands(
                query in ascii_name(),
                candidate in ascii_name(),
            ) {
                let cands = [("id".to_string(), candidate.clone())];
                let score = similarity(&query, &candidate);
                let outcome = find_best_match(&query, &cands, AUTO_MERGE, FLAG);

                match outcome {
                    Some(NormalizeMatch::AutoMerge { score: s, ref entity_id }) => {
                        prop_assert_eq!(entity_id, "id");
                        prop_assert!(s >= AUTO_MERGE, "AutoMerge with score {s} < {AUTO_MERGE}");
                        prop_assert!((s - score).abs() < 1e-9);
                    }
                    Some(NormalizeMatch::Flag { score: s, ref entity_id }) => {
                        prop_assert_eq!(entity_id, "id");
                        prop_assert!(
                            (FLAG..AUTO_MERGE).contains(&s),
                            "Flag with score {s} outside [{FLAG}, {AUTO_MERGE})",
                        );
                        prop_assert!((s - score).abs() < 1e-9);
                    }
                    None => {
                        prop_assert!(
                            score < FLAG,
                            "miss with score {score} >= flag {FLAG} \
                             for ({query:?}, {candidate:?})",
                        );
                    }
                }
            }
        }
    }
}
