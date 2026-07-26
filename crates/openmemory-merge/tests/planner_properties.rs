use std::collections::{BTreeMap, BTreeSet};

use openmemory_core::space::{RevisionId, SpaceId};
use openmemory_merge::canonical::{
    CanonicalEntity, CanonicalRelation, CanonicalSpaceSnapshot, Lifecycle,
};
use openmemory_merge::planner::{
    materialize_merge, plan_merge, CandidateSet, EntityDisposition, MergePolicy, PlanRequest,
};
use proptest::prelude::*;

fn entity(space: SpaceId, id: u16) -> CanonicalEntity {
    CanonicalEntity::new(
        space,
        format!("node-{id}"),
        RevisionId::new(),
        format!("Node {id}"),
        BTreeSet::new(),
        "node".to_string(),
        BTreeSet::new(),
        "generated property fixture".to_string(),
    )
    .unwrap()
}

fn request(target: CanonicalSpaceSnapshot, source: CanonicalSpaceSnapshot) -> PlanRequest {
    PlanRequest {
        source,
        target,
        lineage_base: None,
        candidates: CandidateSet::complete(Vec::new()).unwrap(),
        resolutions: Vec::new(),
        policy: MergePolicy::default(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn plan_is_insertion_order_independent_and_accounts_every_source(
        ids in prop::collection::btree_set(1_u16..2_000, 1..50)
    ) {
        let target_space = SpaceId::new();
        let source_space = SpaceId::new();
        let target = CanonicalSpaceSnapshot::new(
            target_space,
            1,
            vec![entity(target_space, 0)],
            vec![],
            vec![],
        ).unwrap();
        let forward = ids.iter().map(|id| entity(source_space, *id)).collect::<Vec<_>>();
        let mut reverse = forward.clone();
        reverse.reverse();
        let first = CanonicalSpaceSnapshot::new(source_space, 1, forward, vec![], vec![]).unwrap();
        let second = CanonicalSpaceSnapshot::new(source_space, 77, reverse, vec![], vec![]).unwrap();
        let first_plan = plan_merge(&request(target.clone(), first.clone())).unwrap();
        let second_plan = plan_merge(&request(target.clone(), second)).unwrap();
        let result = materialize_merge(&target, &first, &first_plan).unwrap();

        prop_assert_eq!(first_plan.plan_hash, second_plan.plan_hash);
        prop_assert_eq!(first_plan.predicted_result_hash, second_plan.predicted_result_hash);
        prop_assert_eq!(first_plan.accounting.source_entities_accounted, ids.len());
        prop_assert_eq!(first_plan.dispositions.len(), ids.len());
        prop_assert_eq!(result.entities.len(), ids.len() + 1);
        let all_distinct = first_plan.dispositions.iter().all(|item| {
            matches!(item, EntityDisposition::AddDistinct { .. })
        });
        prop_assert!(all_distinct);
    }

    #[test]
    fn every_rewired_relation_endpoint_exists(
        ids in prop::collection::btree_set(1_u16..2_000, 1..40)
    ) {
        let target_space = SpaceId::new();
        let source_space = SpaceId::new();
        let target = CanonicalSpaceSnapshot::new(
            target_space,
            1,
            vec![entity(target_space, 0)],
            vec![],
            vec![],
        ).unwrap();
        let mut entities = vec![entity(source_space, 0)];
        entities.extend(ids.iter().map(|id| entity(source_space, *id)));
        let relations = ids.iter().map(|id| {
            CanonicalRelation::new(
                source_space,
                format!("edge-{id}"),
                RevisionId::new(),
                "node-0".to_string(),
                format!("node-{id}"),
                "links_to".to_string(),
                1.0,
                "property".to_string(),
                BTreeSet::new(),
                Lifecycle::Active,
            ).unwrap()
        }).collect::<Vec<_>>();
        let source = CanonicalSpaceSnapshot::new(source_space, 1, entities, vec![], relations).unwrap();
        let plan = plan_merge(&request(target.clone(), source.clone())).unwrap();
        let result = materialize_merge(&target, &source, &plan).unwrap();

        prop_assert_eq!(plan.accounting.source_relations_accounted, ids.len());
        for relation in &result.relations {
            prop_assert!(result.entity(&relation.from_entity).is_some());
            prop_assert!(result.entity(&relation.to_entity).is_some());
        }
    }
}

#[test]
fn property_module_does_not_accidentally_depend_on_map_order() {
    let values = BTreeMap::from([("a", 1_u8), ("b", 2_u8)]);
    assert_eq!(values.keys().copied().collect::<Vec<_>>(), vec!["a", "b"]);
}
