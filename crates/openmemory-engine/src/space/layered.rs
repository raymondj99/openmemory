//! Deterministic bounded recall across one to four authorized spaces.

use std::collections::BTreeMap;
use std::sync::Arc;

use openmemory_core::space::{SpaceId, SpaceRef, MAX_READ_SET};
use openmemory_graph::recall::{RecallFilters, RecallResult};
use openmemory_graph::{MemoryError, MemoryResult, SearchMode};

use crate::partition::DomainStore;

const DEFAULT_CANDIDATE_MULTIPLIER: usize = 2;
const MAX_COMPONENT_CANDIDATES: usize = 256;
const FUSION_POLICY_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct SpaceReadHandle {
    pub space: SpaceRef,
    pub read_priority: u8,
    pub domains: Arc<DomainStore>,
}

#[derive(Debug, Clone)]
pub struct LayeredRecallRequest {
    pub query: String,
    pub top_k: usize,
    pub filters: RecallFilters,
    pub candidate_multiplier: usize,
    /// Product policy must opt in explicitly; semantic contexts fail closed.
    pub allow_partial: bool,
}

impl LayeredRecallRequest {
    #[must_use]
    pub fn new(query: impl Into<String>, top_k: usize, filters: RecallFilters) -> Self {
        Self {
            query: query.into(),
            top_k,
            filters,
            candidate_multiplier: DEFAULT_CANDIDATE_MULTIPLIER,
            allow_partial: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallOrigin {
    pub space_id: SpaceId,
    pub logical_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScopedRecallResult {
    pub space: SpaceRef,
    pub read_priority: u8,
    pub local: RecallResult,
    pub layer_prior: f32,
    pub adjusted_score: f32,
    pub semantic_revision_hash: Option<Vec<u8>>,
    pub duplicate_origins: Vec<RecallOrigin>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayeredRecallResponse {
    pub results: Vec<ScopedRecallResult>,
    pub missing_spaces: Vec<SpaceId>,
    pub fusion_policy_version: u32,
}

/// Recall with a zero-overhead single-space branch and bounded 2–4 component
/// fan-out. Component completion order never influences output.
pub fn layered_recall(
    read_set: &[SpaceReadHandle],
    request: &LayeredRecallRequest,
) -> MemoryResult<LayeredRecallResponse> {
    validate(read_set, request)?;
    let component_top_k = request
        .top_k
        .saturating_mul(request.candidate_multiplier.max(1))
        .clamp(1, MAX_COMPONENT_CANDIDATES);

    if read_set.len() == 1 {
        let handle = &read_set[0];
        let hits = handle
            .domains
            .recall(&request.query, component_top_k, &request.filters)?;
        return fuse(
            read_set,
            vec![Ok(hits)],
            request.top_k,
            request.allow_partial,
        );
    }

    // Keyword reads over two roots are normally cheaper sequentially than
    // starting another scheduler. Vector/3–4 component reads use at most
    // three scoped workers plus the caller.
    let mode = request.filters.mode.unwrap_or_default();
    let component_results = if read_set.len() == 2 && mode == SearchMode::KeywordOnly {
        read_set
            .iter()
            .map(|handle| {
                handle
                    .domains
                    .recall(&request.query, component_top_k, &request.filters)
            })
            .collect()
    } else {
        let (first, rest) = read_set.split_first().expect("validated non-empty");
        std::thread::scope(|scope| {
            let workers: Vec<_> = rest
                .iter()
                .map(|handle| {
                    scope.spawn(|| {
                        handle
                            .domains
                            .recall(&request.query, component_top_k, &request.filters)
                    })
                })
                .collect();
            let mut results =
                vec![first
                    .domains
                    .recall(&request.query, component_top_k, &request.filters)];
            results.extend(workers.into_iter().map(|worker| match worker.join() {
                Ok(result) => result,
                Err(_) => Err(MemoryError::InvalidInput(
                    "layered recall worker panicked".to_string(),
                )),
            }));
            results
        })
    };

    fuse(
        read_set,
        component_results,
        request.top_k,
        request.allow_partial,
    )
}

fn validate(read_set: &[SpaceReadHandle], request: &LayeredRecallRequest) -> MemoryResult<()> {
    if read_set.is_empty() || read_set.len() > MAX_READ_SET {
        return Err(MemoryError::InvalidInput(format!(
            "layered recall requires 1..={MAX_READ_SET} spaces"
        )));
    }
    if request.query.trim().is_empty() {
        return Err(MemoryError::InvalidInput(
            "layered recall query cannot be empty".to_string(),
        ));
    }
    if request.top_k == 0 || request.top_k > MAX_COMPONENT_CANDIDATES {
        return Err(MemoryError::InvalidInput(
            "layered recall top_k must be in 1..=256".to_string(),
        ));
    }
    let mut ids: Vec<_> = read_set.iter().map(|handle| handle.space.id).collect();
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(MemoryError::InvalidInput(
            "layered recall contains duplicate spaces".to_string(),
        ));
    }
    if read_set
        .iter()
        .any(|handle| handle.domains.space_id() != handle.space.id)
    {
        return Err(MemoryError::InvalidInput(
            "layered recall handle has a mismatched physical space".to_string(),
        ));
    }
    Ok(())
}

fn fuse(
    read_set: &[SpaceReadHandle],
    component_results: Vec<MemoryResult<Vec<RecallResult>>>,
    top_k: usize,
    allow_partial: bool,
) -> MemoryResult<LayeredRecallResponse> {
    let mut all = Vec::new();
    let mut missing = Vec::new();
    for (handle, result) in read_set.iter().zip(component_results) {
        let hits = match result {
            Ok(hits) => hits,
            Err(error) if allow_partial => {
                missing.push(handle.space.id);
                tracing::warn!(space_id = %handle.space.id, %error, "layered recall component unavailable");
                continue;
            }
            Err(error) => return Err(error),
        };
        for local in hits {
            let hash = handle
                .domains
                .observation_revision_hash(&local.observation.id)?;
            all.push(ScopedRecallResult {
                space: handle.space.clone(),
                read_priority: handle.read_priority,
                adjusted_score: local.score,
                layer_prior: 1.0,
                local,
                semantic_revision_hash: hash,
                duplicate_origins: Vec::new(),
            });
        }
    }

    all.sort_by(result_order);
    let mut winners: Vec<ScopedRecallResult> = Vec::with_capacity(all.len());
    let mut exact: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    for result in all {
        if let Some(hash) = result.semantic_revision_hash.as_ref() {
            if let Some(existing) = exact.get(hash).copied() {
                winners[existing].duplicate_origins.push(RecallOrigin {
                    space_id: result.space.id,
                    logical_id: result.local.observation.id,
                });
                continue;
            }
            exact.insert(hash.clone(), winners.len());
        }
        winners.push(result);
    }
    winners.truncate(top_k);
    for result in &mut winners {
        result.duplicate_origins.sort_by(|left, right| {
            left.space_id
                .cmp(&right.space_id)
                .then_with(|| left.logical_id.cmp(&right.logical_id))
        });
    }
    Ok(LayeredRecallResponse {
        results: winners,
        missing_spaces: missing,
        fusion_policy_version: FUSION_POLICY_VERSION,
    })
}

fn result_order(left: &ScopedRecallResult, right: &ScopedRecallResult) -> std::cmp::Ordering {
    right
        .adjusted_score
        .total_cmp(&left.adjusted_score)
        .then_with(|| left.read_priority.cmp(&right.read_priority))
        .then_with(|| {
            right
                .local
                .observation
                .valid_from
                .cmp(&left.local.observation.valid_from)
        })
        .then_with(|| {
            right
                .local
                .observation
                .observed_at
                .cmp(&left.local.observation.observed_at)
        })
        .then_with(|| left.space.id.cmp(&right.space.id))
        .then_with(|| left.local.observation.id.cmp(&right.local.observation.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmemory_core::config::Config;
    use openmemory_core::space::{PrincipalId, SpaceContext, SpaceOwner};
    use openmemory_graph::{EntityType, ObservationInput};

    fn read_handle(name: &str, priority: u8) -> (SpaceReadHandle, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let id = SpaceId::new();
        let domains =
            Arc::new(DomainStore::open_scoped(&Config::default(), root.path(), 1, id).unwrap());
        domains
            .remember(
                name,
                EntityType::Fact,
                &[ObservationInput::new(format!("{name} layered needle"))],
                &[],
                "test",
            )
            .unwrap();
        let owner: PrincipalId = "local:test".parse().unwrap();
        (
            SpaceReadHandle {
                space: SpaceRef {
                    id,
                    owner: SpaceOwner::User(owner),
                    context: SpaceContext::Global,
                },
                read_priority: priority,
                domains,
            },
            root,
        )
    }

    #[test]
    fn layered_results_are_stable_by_priority_not_completion() {
        let (first, _first_root) = read_handle("first", 0);
        let (second, _second_root) = read_handle("second", 1);
        let request = LayeredRecallRequest::new("layered needle", 10, RecallFilters::new());
        let response = layered_recall(&[second.clone(), first.clone()], &request).unwrap();
        assert_eq!(response.results.len(), 2);
        assert_eq!(response.results[0].read_priority, 0);
        assert!(response.missing_spaces.is_empty());
    }

    #[test]
    fn mismatched_physical_space_fails_closed() {
        let (mut first, _root) = read_handle("first", 0);
        first.space.id = SpaceId::new();
        let request = LayeredRecallRequest::new("needle", 10, RecallFilters::new());
        assert!(layered_recall(&[first], &request).is_err());
    }
}
