//! Fuzzy entity-name normalization for the `remember` write path.
//!
//! When `ensure_entity` fails to find an exact `(name, entity_type)` match,
//! the normalizer scores the incoming name against recent entities of the
//! same type. Scoring uses two fast-path equality checks (stripped and
//! token-sorted forms) plus a conservative slow path that requires both
//! Jaro-Winkler and normalized Levenshtein to agree, preventing
//! prefix-biased false positives on short names like "Topic1" / "Topic10".

use strsim::{jaro_winkler, normalized_levenshtein};

#[derive(Debug, Clone, PartialEq)]
pub enum NormalizeMatch {
    AutoMerge { entity_id: String, score: f64 },
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

pub fn similarity(a: &str, b: &str) -> f64 {
    let a_low = a.to_lowercase();
    let b_low = b.to_lowercase();
    let a_stripped = alnum_only(&a_low);
    let b_stripped = alnum_only(&b_low);

    // Fast path: identical after stripping punctuation/spaces
    if a_stripped == b_stripped {
        return 1.0;
    }

    // Fast path: identical after word reordering
    if token_sort(&a_low) == token_sort(&b_low) {
        return 1.0;
    }

    // Slow path: both metrics must agree to prevent prefix-biased false
    // positives. JW is prefix-weighted and over-scores "Topic1" vs "Topic10";
    // NLev penalizes edits proportionally and corrects for this.
    let jw = jaro_winkler(&token_sort(&a_low), &token_sort(&b_low))
        .max(jaro_winkler(&a_stripped, &b_stripped));
    let nlev = normalized_levenshtein(&a_stripped, &b_stripped);
    jw.min(nlev)
}

pub fn find_best_match(
    name: &str,
    candidates: &[(String, String)],
    auto_merge_threshold: f64,
    flag_threshold: f64,
) -> Option<NormalizeMatch> {
    let mut best_id = None;
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

    #[test]
    fn exact_casing() {
        let score = similarity("ProjectAlpha", "projectalpha");
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn spacing_concat() {
        let score = similarity("ProjectAlpha", "Project Alpha");
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn word_order() {
        let score = similarity("Project Alpha", "Alpha Project");
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn hyphenation() {
        let score = similarity("open-memory", "OpenMemory");
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn dot_notation() {
        let score = similarity("React.js", "reactjs");
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn prefix_match() {
        // NLev penalizes the 2-char truncation; flags rather than auto-merges.
        let score = similarity("PostgreSQL", "Postgres");
        assert!(score >= 0.75, "expected >= 0.75, got {score}");
        assert!(score < 0.85, "expected < 0.85, got {score}");
    }

    #[test]
    fn near_miss() {
        // JW.min(NLev) drops this below the flag threshold.
        let score = similarity("sift", "swift");
        assert!(score >= 0.75, "expected >= 0.75, got {score}");
        assert!(score < 0.85, "expected < 0.85, got {score}");
    }

    #[test]
    fn prefix_sharing_no_auto_merge() {
        // "Topic1" vs "Topic10" must NOT auto-merge despite high JW score.
        let score = similarity("Topic1", "Topic10");
        assert!(score < 0.95, "expected < 0.95 (no auto-merge), got {score}");
    }

    #[test]
    fn abbreviation_no_match() {
        let score = similarity("ML Pipeline", "Machine Learning Pipeline");
        assert!(score < 0.85, "expected < 0.85, got {score}");
    }

    #[test]
    fn unrelated_no_match() {
        let score = similarity("Raymond", "Kubernetes");
        assert!(score < 0.85, "expected < 0.85, got {score}");
    }

    #[test]
    fn find_best_match_auto_merge() {
        let candidates = vec![
            ("id1".into(), "Project Alpha".into()),
            ("id2".into(), "Something Else".into()),
        ];
        let result = find_best_match("project alpha", &candidates, 0.95, 0.85);
        assert!(matches!(
            result,
            Some(NormalizeMatch::AutoMerge { ref entity_id, .. }) if entity_id == "id1"
        ));
    }

    #[test]
    fn find_best_match_flag() {
        let candidates = vec![("id1".into(), "config".into())];
        let result = find_best_match("configs", &candidates, 0.95, 0.85);
        assert!(matches!(
            result,
            Some(NormalizeMatch::Flag { ref entity_id, .. }) if entity_id == "id1"
        ));
    }

    #[test]
    fn find_best_match_none() {
        let candidates = vec![("id1".into(), "Raymond".into())];
        let result = find_best_match("Kubernetes", &candidates, 0.95, 0.85);
        assert!(result.is_none());
    }

    #[test]
    fn find_best_match_empty_candidates() {
        let result = find_best_match("anything", &[], 0.95, 0.85);
        assert!(result.is_none());
    }
}
