use std::collections::{BTreeMap, BTreeSet};

use openmemory_memory_model_poc::identity::IdentityDecision;
use openmemory_memory_model_poc::semantic_merge::{
    materialize_knowledge_merge, plan_knowledge_merge, CandidatePair, IdentityResolution,
    KnowledgeSnapshot, LocalEntity, LocalRelation,
};
use proptest::prelude::*;

fn entity(id: String, revision: String, source: String) -> LocalEntity {
    LocalEntity {
        label: id.clone(),
        logical_id: id,
        revision_id: revision,
        aliases: BTreeSet::new(),
        kind: "node".to_string(),
        identifiers: BTreeMap::new(),
        description: "generated property-test entity".to_string(),
        source_refs: BTreeSet::from([source]),
    }
}

fn target() -> KnowledgeSnapshot {
    KnowledgeSnapshot::from_local(
        "target",
        "target-r1",
        vec![entity(
            "root".to_string(),
            "root-r1".to_string(),
            "root-source".to_string(),
        )],
        vec![],
    )
    .expect("target is valid")
}

proptest! {
    #[test]
    fn every_source_entity_is_accounted_once_independent_of_input_order(
        ids in prop::collection::btree_set(0_u16..2_000, 1..80)
    ) {
        let target = target();
        let forward = ids
            .iter()
            .map(|id| entity(format!("node-{id}"), format!("revision-{id}"), format!("source-{id}")))
            .collect::<Vec<_>>();
        let mut reverse = forward.clone();
        reverse.reverse();
        let first = KnowledgeSnapshot::from_local("source", "source-r1", forward, vec![])
            .expect("first source is valid");
        let second = KnowledgeSnapshot::from_local("source", "source-r1", reverse, vec![])
            .expect("second source is valid");

        let first_plan = plan_knowledge_merge(&target, &first, &BTreeSet::new(), &[])
            .expect("first plan succeeds");
        let second_plan = plan_knowledge_merge(&target, &second, &BTreeSet::new(), &[])
            .expect("second plan succeeds");
        let result = materialize_knowledge_merge(&target, &first, &first_plan)
            .expect("materialization succeeds");

        prop_assert_eq!(first.hash(), second.hash());
        prop_assert_eq!(&first_plan.plan_hash, &second_plan.plan_hash);
        prop_assert_eq!(first_plan.entity_actions.len(), ids.len());
        prop_assert_eq!(first_plan.added_entities(), ids.len());
        prop_assert_eq!(
            result.entities().values().map(|entity| entity.contributions.len()).sum::<usize>(),
            ids.len() + 1
        );
    }

    #[test]
    fn rewired_relation_endpoints_always_exist(
        ids in prop::collection::btree_set(1_u16..2_000, 1..60)
    ) {
        let target = target();
        let mut entities = vec![entity(
            "source-root".to_string(),
            "source-root-r1".to_string(),
            "source-root-ref".to_string(),
        )];
        entities.extend(ids.iter().map(|id| {
            entity(format!("node-{id}"), format!("revision-{id}"), format!("source-{id}"))
        }));
        let relations = ids
            .iter()
            .map(|id| LocalRelation {
                subject: "source-root".to_string(),
                predicate: "links_to".to_string(),
                object: format!("node-{id}"),
                source_ref: "source-root-ref".to_string(),
            })
            .collect::<Vec<_>>();
        let source = KnowledgeSnapshot::from_local("source", "source-r1", entities, relations)
            .expect("source is valid");
        let pair = CandidatePair::new("root", "source-root");
        let resolution = IdentityResolution::reviewed(
            pair.clone(),
            IdentityDecision::Same,
            &target.entities()["root"].revision_id,
            &source.entities()["source-root"].revision_id,
            "property-packet",
            "property-reviewer",
        )
        .expect("resolution is valid");
        let plan = plan_knowledge_merge(
            &target,
            &source,
            &BTreeSet::from([pair]),
            &[resolution],
        )
        .expect("plan succeeds");
        let result = materialize_knowledge_merge(&target, &source, &plan)
            .expect("materialization succeeds");

        prop_assert_eq!(plan.relation_actions.len(), ids.len());
        for relation in result.relations().keys() {
            prop_assert!(result.entities().contains_key(&relation.subject));
            prop_assert!(result.entities().contains_key(&relation.object));
        }
    }
}
