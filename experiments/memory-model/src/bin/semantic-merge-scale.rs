use std::collections::{BTreeMap, BTreeSet};
use std::hint::black_box;
use std::time::Instant;

use openmemory_memory_model_poc::semantic_merge::{
    materialize_knowledge_merge, plan_knowledge_merge, KnowledgeSnapshot, LocalEntity,
    LocalRelation,
};

fn entity(id: String, revision: String, source: String) -> LocalEntity {
    LocalEntity {
        label: id.clone(),
        logical_id: id,
        revision_id: revision,
        aliases: BTreeSet::new(),
        kind: "benchmark_node".to_string(),
        identifiers: BTreeMap::new(),
        description: "scale benchmark entity".to_string(),
        source_refs: BTreeSet::from([source]),
    }
}

fn fixture(size: usize) -> (KnowledgeSnapshot, KnowledgeSnapshot) {
    let target = KnowledgeSnapshot::from_local(
        "target",
        "target-r1",
        vec![entity(
            "root".to_string(),
            "root-r1".to_string(),
            "root-source".to_string(),
        )],
        vec![],
    )
    .expect("target is valid");
    let entities = (0..size)
        .map(|index| {
            entity(
                format!("node-{index:05}"),
                format!("revision-{index}"),
                format!("source-{index}"),
            )
        })
        .collect::<Vec<_>>();
    let relations = (0..size.saturating_sub(1))
        .map(|index| LocalRelation {
            subject: format!("node-{index:05}"),
            predicate: "next".to_string(),
            object: format!("node-{:05}", index + 1),
            source_ref: format!("source-{index}"),
        })
        .collect::<Vec<_>>();
    let source = KnowledgeSnapshot::from_local("source", "source-r1", entities, relations)
        .expect("source is valid");
    (target, source)
}

fn measure(size: usize, iterations: u32) {
    let (target, source) = fixture(size);
    let candidates = BTreeSet::new();
    let warmup =
        plan_knowledge_merge(&target, &source, &candidates, &[]).expect("warm-up plan succeeds");
    black_box(
        materialize_knowledge_merge(&target, &source, &warmup)
            .expect("warm-up materialization succeeds"),
    );

    let started = Instant::now();
    for _ in 0..iterations {
        let plan = black_box(
            plan_knowledge_merge(&target, &source, &candidates, &[])
                .expect("measured plan succeeds"),
        );
        black_box(
            materialize_knowledge_merge(&target, &source, &plan)
                .expect("measured materialization succeeds"),
        );
    }
    let elapsed = started.elapsed();
    println!(
        "entities={size} relations={} iterations={iterations} mean_ms={:.3}",
        size.saturating_sub(1),
        elapsed.as_secs_f64() * 1_000.0 / f64::from(iterations)
    );
}

fn main() {
    measure(100, 1_000);
    measure(1_000, 100);
    measure(10_000, 10);
}
