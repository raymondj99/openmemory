//! Layered cross-space recall composition.
//!
//! Invariant (established by measurement; see plan/16 INDEX "Amendments
//! from measured evidence"): per-space scores are NEVER compared across
//! spaces. Raw-score sorting across corpora of different ages gave one
//! space 0.0% of fused positions across 839 queries. The provisional
//! composition policy is deterministic rank interleaving: results are
//! taken round-robin in read-set order by within-space rank, so every
//! space in the read set contributes its best results regardless of how
//! its scores are calibrated.

use openmemory_graph::RecallResult;

/// Maximum spaces in one ordered read set (plan invariant: a request has
/// one authorized ordered read set of at most four spaces).
pub const MAX_READ_SPACES: usize = 4;

/// One fused hit: the contributing space (`None` = personal-global
/// default), the hit's rank within that space (0-based), and the result.
#[derive(Debug, Clone)]
pub struct LayeredHit {
    pub space: Option<String>,
    pub rank_in_space: usize,
    pub result: RecallResult,
}

/// Fuse per-space ranked lists by deterministic rank interleaving.
///
/// Round `r` takes the rank-`r` result from each space in read-set
/// order, until `limit` results are collected or every list is
/// exhausted. Scores are carried through untouched for display but play
/// no part in ordering across spaces.
#[must_use]
pub fn interleave_by_rank(
    lists: Vec<(Option<String>, Vec<RecallResult>)>,
    limit: usize,
) -> Vec<LayeredHit> {
    let mut out = Vec::new();
    if limit == 0 || lists.is_empty() {
        return out;
    }
    let deepest = lists.iter().map(|(_, l)| l.len()).max().unwrap_or(0);
    let mut lists: Vec<(Option<String>, std::vec::IntoIter<RecallResult>)> = lists
        .into_iter()
        .map(|(space, list)| (space, list.into_iter()))
        .collect();
    for rank in 0..deepest {
        for (space, iter) in &mut lists {
            if let Some(result) = iter.next() {
                out.push(LayeredHit {
                    space: space.clone(),
                    rank_in_space: rank,
                    result,
                });
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmemory_graph::{EntityType, Observation, RecallResult};

    fn hit(id: &str, score: f32) -> RecallResult {
        let mut observation = Observation::new("e", format!("content {id}"), 0);
        observation.id = id.to_owned();
        RecallResult {
            observation,
            entity_name: "e".to_owned(),
            entity_type: EntityType::Fact,
            score,
            raw_score: score,
        }
    }

    #[test]
    fn interleaves_in_read_set_order_by_rank() {
        let fused = interleave_by_rank(
            vec![
                (Some("a".into()), vec![hit("a0", 0.1), hit("a1", 0.1)]),
                (Some("b".into()), vec![hit("b0", 99.0), hit("b1", 99.0)]),
            ],
            10,
        );
        let ids: Vec<_> = fused
            .iter()
            .map(|h| h.result.observation.id.as_str())
            .collect();
        // b's enormous scores must not reorder anything: rank 0 of each
        // space first (read-set order), then rank 1 of each.
        assert_eq!(ids, vec!["a0", "b0", "a1", "b1"]);
        assert_eq!(fused[0].space.as_deref(), Some("a"));
        assert_eq!(fused[1].rank_in_space, 0);
        assert_eq!(fused[2].rank_in_space, 1);
    }

    #[test]
    fn exhausted_space_yields_to_deeper_lists() {
        let fused = interleave_by_rank(
            vec![
                (None, vec![hit("d0", 1.0)]),
                (
                    Some("s".into()),
                    vec![hit("s0", 1.0), hit("s1", 1.0), hit("s2", 1.0)],
                ),
            ],
            10,
        );
        let ids: Vec<_> = fused
            .iter()
            .map(|h| h.result.observation.id.as_str())
            .collect();
        assert_eq!(ids, vec!["d0", "s0", "s1", "s2"]);
    }

    #[test]
    fn respects_limit() {
        let fused = interleave_by_rank(
            vec![
                (Some("a".into()), vec![hit("a0", 1.0), hit("a1", 1.0)]),
                (Some("b".into()), vec![hit("b0", 1.0), hit("b1", 1.0)]),
            ],
            3,
        );
        assert_eq!(fused.len(), 3);
    }

    #[test]
    fn empty_inputs_produce_empty_output() {
        assert!(interleave_by_rank(vec![], 5).is_empty());
        assert!(interleave_by_rank(vec![(None, vec![])], 5).is_empty());
        assert!(interleave_by_rank(vec![(None, vec![hit("x", 1.0)])], 0).is_empty());
    }
}
