use std::collections::{BTreeMap, BTreeSet};

use openmemory_memory_model_poc::semantic_merge::{
    materialize_knowledge_merge, plan_knowledge_merge, KnowledgeSnapshot, LocalEntity,
    LocalRelation,
};

#[test]
fn ten_thousand_entity_chain_is_fully_accounted() {
    const SIZE: usize = 10_000;
    let target = KnowledgeSnapshot::from_local(
        "target",
        "target-r1",
        vec![local_entity(0, "target-root")],
        vec![],
    )
    .expect("target is valid");
    let source_entities = (0..SIZE)
        .map(|index| local_entity(index, "source"))
        .collect::<Vec<_>>();
    let source_relations = (0..SIZE - 1)
        .map(|index| LocalRelation {
            subject: format!("source-{index}"),
            predicate: "next".to_string(),
            object: format!("source-{}", index + 1),
            source_ref: format!("source-ref-{index}"),
        })
        .collect::<Vec<_>>();
    let source = KnowledgeSnapshot::from_local(
        "source-space",
        "source-r1",
        source_entities,
        source_relations,
    )
    .expect("source is valid");

    let plan =
        plan_knowledge_merge(&target, &source, &BTreeSet::new(), &[]).expect("large merge plans");
    let result =
        materialize_knowledge_merge(&target, &source, &plan).expect("large merge materializes");

    assert_eq!(plan.entity_actions.len(), SIZE);
    assert_eq!(plan.added_entities(), SIZE);
    assert_eq!(plan.relation_actions.len(), SIZE - 1);
    assert_eq!(result.entities().len(), SIZE + 1);
    assert_eq!(result.relation_assertion_count(), SIZE - 1);
}

fn local_entity(index: usize, prefix: &str) -> LocalEntity {
    LocalEntity {
        logical_id: format!("{prefix}-{index}"),
        revision_id: format!("revision-{prefix}-{index}"),
        label: format!("node {index}"),
        aliases: BTreeSet::new(),
        kind: "scale_node".to_string(),
        identifiers: BTreeMap::new(),
        description: "large deterministic scale fixture".to_string(),
        source_refs: BTreeSet::from([format!("{prefix}-ref-{index}")]),
    }
}
