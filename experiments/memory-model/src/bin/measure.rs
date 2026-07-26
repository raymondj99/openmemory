use std::hint::black_box;
use std::path::Path;
use std::time::{Duration, Instant};

use openmemory_core::config::Config;
use openmemory_graph::{
    EntityType, Observation, ObservationInput, RecallFilters, RecallResult, SearchMode,
};
use openmemory_memory_model_poc::audit::{
    AuditStore, ChangeSetDraft, DraftOp, MemorySnapshot, SubmitMode,
};
use openmemory_memory_model_poc::merge::{materialize_plan, plan_merge, GraphSnapshot};
use openmemory_memory_model_poc::spaces::{
    descriptor, merge_layered_hits, LayeredRecallHit, MemorySpace, ReadSet, SpaceContext, SpaceId,
    SpaceOwner, SpaceRegistry,
};
use openmemory_memory_model_poc::PocResult;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct Stats {
    samples: usize,
    mean_us: u128,
    p50_us: u128,
    p95_us: u128,
    p99_us: u128,
    max_us: u128,
}

#[derive(Debug, Serialize)]
struct AuditMeasurements {
    direct_add: Stats,
    audited_apply: Stats,
    compact_immutable_audit: Stats,
    proposed_change: Stats,
    audited_overhead_p95_percent: i128,
    compact_overhead_p95_percent: i128,
    direct_durable_bytes: u64,
    audited_durable_bytes: u64,
    compact_durable_bytes: u64,
    requested_json_bytes: u64,
    before_after_and_hash_bytes: u64,
}

#[derive(Debug, Serialize)]
struct RecallMeasurements {
    one_space: Stats,
    two_space_sequential: Stats,
    two_space_parallel: Stats,
    two_space_parallel_with_10k_closed: Stats,
    merge_512_candidates: Stats,
    two_space_budget_us: u128,
    two_space_budget_passed: bool,
    candidate_merge_budget_us: u128,
    candidate_merge_budget_passed: bool,
}

#[derive(Debug, Serialize)]
struct MergeMeasurements {
    plan_10k_memories: Stats,
    clean_actions: usize,
    conflicts: usize,
    materialize_1k_memories_us: u128,
}

#[derive(Debug, Serialize)]
struct Report {
    audit: AuditMeasurements,
    recall: RecallMeasurements,
    merge: MergeMeasurements,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("measurement failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> PocResult<()> {
    let root = tempfile::tempdir()?;
    let report = Report {
        audit: measure_audit(&root.path().join("audit"))?,
        recall: measure_recall(&root.path().join("recall"))?,
        merge: measure_merge(&root.path().join("merge"))?,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn measure(mut operation: impl FnMut()) -> Stats {
    let mut durations = Vec::new();
    for _ in 0..10 {
        operation();
    }
    for _ in 0..200 {
        let start = Instant::now();
        operation();
        durations.push(start.elapsed());
    }
    stats(&mut durations)
}

fn measure_n(samples: usize, mut operation: impl FnMut()) -> Stats {
    let mut durations = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        operation();
        durations.push(start.elapsed());
    }
    stats(&mut durations)
}

fn stats(durations: &mut [Duration]) -> Stats {
    durations.sort_unstable();
    let micros = |index: usize| durations[index.min(durations.len() - 1)].as_micros();
    let percentile = |percent: usize| (durations.len() * percent).div_ceil(100).saturating_sub(1);
    Stats {
        samples: durations.len(),
        mean_us: durations.iter().map(Duration::as_nanos).sum::<u128>()
            / u128::try_from(durations.len()).expect("sample count fits u128")
            / 1_000,
        p50_us: micros(percentile(50)),
        p95_us: micros(percentile(95)),
        p99_us: micros(percentile(99)),
        max_us: micros(durations.len() - 1),
    }
}

fn measure_audit(root: &Path) -> PocResult<AuditMeasurements> {
    let direct = AuditStore::open(&root.join("direct"))?;
    let audited = AuditStore::open(&root.join("audited"))?;
    let compact = AuditStore::open(&root.join("compact"))?;
    let proposed = AuditStore::open(&root.join("proposed"))?;
    let stores = AuditBenchStores {
        direct: &direct,
        audited: &audited,
        compact: &compact,
        proposed: &proposed,
    };
    for index in 0..20 {
        for variant in AUDIT_ORDERS[0] {
            audit_operation(&stores, variant, format!("warm-{index}-{variant:?}"));
        }
    }
    let mut samples = AuditSamples::default();
    for index in 0..800 {
        for variant in AUDIT_ORDERS[index % AUDIT_ORDERS.len()] {
            let id = format!("measure-{index}-{variant:?}");
            let started = Instant::now();
            audit_operation(&stores, variant, id);
            samples.for_variant(variant).push(started.elapsed());
        }
    }
    let direct_stats = stats(&mut samples.direct);
    let audited_stats = stats(&mut samples.audited);
    let compact_stats = stats(&mut samples.compact);
    let proposed_stats = stats(&mut samples.proposed);
    let (requested, diff) = audited.ledger_payload_bytes_for_benchmark()?;
    let overhead =
        i128::try_from(audited_stats.p95_us.saturating_mul(100) / direct_stats.p95_us.max(1))
            .unwrap_or(i128::MAX)
            - 100;
    let compact_overhead =
        i128::try_from(compact_stats.p95_us.saturating_mul(100) / direct_stats.p95_us.max(1))
            .unwrap_or(i128::MAX)
            - 100;
    Ok(AuditMeasurements {
        direct_add: direct_stats,
        audited_apply: audited_stats,
        compact_immutable_audit: compact_stats,
        proposed_change: proposed_stats,
        audited_overhead_p95_percent: overhead,
        compact_overhead_p95_percent: compact_overhead,
        direct_durable_bytes: direct.durable_bytes_for_benchmark()?,
        audited_durable_bytes: audited.durable_bytes_for_benchmark()?,
        compact_durable_bytes: compact.durable_bytes_for_benchmark()?,
        requested_json_bytes: requested,
        before_after_and_hash_bytes: diff,
    })
}

#[derive(Debug, Clone, Copy)]
enum AuditVariant {
    Direct,
    Full,
    Compact,
    Proposed,
}

const AUDIT_ORDERS: [[AuditVariant; 4]; 4] = [
    [
        AuditVariant::Direct,
        AuditVariant::Full,
        AuditVariant::Compact,
        AuditVariant::Proposed,
    ],
    [
        AuditVariant::Full,
        AuditVariant::Compact,
        AuditVariant::Proposed,
        AuditVariant::Direct,
    ],
    [
        AuditVariant::Compact,
        AuditVariant::Proposed,
        AuditVariant::Direct,
        AuditVariant::Full,
    ],
    [
        AuditVariant::Proposed,
        AuditVariant::Direct,
        AuditVariant::Full,
        AuditVariant::Compact,
    ],
];

struct AuditBenchStores<'a> {
    direct: &'a AuditStore,
    audited: &'a AuditStore,
    compact: &'a AuditStore,
    proposed: &'a AuditStore,
}

#[derive(Default)]
struct AuditSamples {
    direct: Vec<Duration>,
    audited: Vec<Duration>,
    compact: Vec<Duration>,
    proposed: Vec<Duration>,
}

impl AuditSamples {
    fn for_variant(&mut self, variant: AuditVariant) -> &mut Vec<Duration> {
        match variant {
            AuditVariant::Direct => &mut self.direct,
            AuditVariant::Full => &mut self.audited,
            AuditVariant::Compact => &mut self.compact,
            AuditVariant::Proposed => &mut self.proposed,
        }
    }
}

fn audit_operation(stores: &AuditBenchStores<'_>, variant: AuditVariant, id: String) {
    match variant {
        AuditVariant::Direct => stores
            .direct
            .direct_add_for_benchmark(&id, "small benchmark memory")
            .expect("direct benchmark add"),
        AuditVariant::Full | AuditVariant::Proposed => {
            stores
                .audited_store(variant)
                .submit(
                    &ChangeSetDraft::new(
                        format!("audit-key-{id}"),
                        "benchmark",
                        "small audited write",
                        vec![DraftOp::Add {
                            logical_id: id,
                            content: "small benchmark memory".to_string(),
                        }],
                    ),
                    if matches!(variant, AuditVariant::Full) {
                        SubmitMode::Apply
                    } else {
                        SubmitMode::Propose
                    },
                )
                .expect("full audit benchmark add");
        }
        AuditVariant::Compact => stores
            .compact
            .compact_audited_add_for_benchmark(
                &format!("compact-key-{id}"),
                &id,
                "small benchmark memory",
            )
            .expect("compact audited benchmark add"),
    };
}

impl AuditBenchStores<'_> {
    fn audited_store(&self, variant: AuditVariant) -> &AuditStore {
        if matches!(variant, AuditVariant::Proposed) {
            self.proposed
        } else {
            self.audited
        }
    }
}

fn open_space(root: &Path, id: &str) -> PocResult<MemorySpace> {
    let id = SpaceId::parse(id)?;
    MemorySpace::open(
        &Config::default(),
        descriptor(
            id,
            SpaceOwner::User("benchmark".to_string()),
            SpaceContext::Project("benchmark".to_string()),
            root,
        ),
    )
}

fn measure_recall(root: &Path) -> PocResult<RecallMeasurements> {
    let alpha = open_space(&root.join("alpha"), "alpha")?;
    let beta = open_space(&root.join("beta"), "beta")?;
    let observations = (0..256)
        .map(|index| ObservationInput::new(format!("validationneedle fact number {index}")))
        .collect::<Vec<_>>();
    alpha.store().remember(
        "Alpha benchmark",
        EntityType::Project,
        &observations,
        &[],
        "benchmark",
    )?;
    beta.store().remember(
        "Beta benchmark",
        EntityType::Project,
        &observations,
        &[],
        "benchmark",
    )?;
    let mut registry = SpaceRegistry::new();
    registry.insert_open(alpha)?;
    registry.insert_open(beta)?;
    let one = ReadSet::new(vec![SpaceId::parse("alpha")?])?;
    let two = ReadSet::new(vec![SpaceId::parse("alpha")?, SpaceId::parse("beta")?])?;
    let filters = RecallFilters {
        mode: Some(SearchMode::KeywordOnly),
        spreading_activation: false,
        record_access: false,
        ..RecallFilters::default()
    };
    let one_stats = measure(|| {
        black_box(
            registry
                .recall_parallel(&one, "validationneedle", 20, &filters)
                .expect("one-space recall"),
        );
    });
    let sequential_stats = measure(|| {
        black_box(
            registry
                .recall_sequential(&two, "validationneedle", 20, &filters)
                .expect("sequential recall"),
        );
    });
    let parallel_stats = measure(|| {
        black_box(
            registry
                .recall_parallel(&two, "validationneedle", 20, &filters)
                .expect("parallel recall"),
        );
    });
    register_closed_spaces(&mut registry, root)?;
    let large_catalog_stats = measure(|| {
        black_box(
            registry
                .recall_parallel(&two, "validationneedle", 20, &filters)
                .expect("large catalog recall"),
        );
    });

    let candidates = synthetic_candidates()?;
    let merge_stats = measure(|| {
        black_box(merge_layered_hits(candidates.clone(), 128));
    });
    let two_space_budget = one_stats.p95_us.saturating_mul(5) / 4 + 5_000;
    Ok(RecallMeasurements {
        one_space: one_stats,
        two_space_sequential: sequential_stats,
        two_space_budget_passed: parallel_stats.p95_us <= two_space_budget,
        two_space_parallel: parallel_stats,
        two_space_parallel_with_10k_closed: large_catalog_stats,
        candidate_merge_budget_passed: merge_stats.p95_us < 2_000,
        merge_512_candidates: merge_stats,
        two_space_budget_us: two_space_budget,
        candidate_merge_budget_us: 2_000,
    })
}

fn register_closed_spaces(registry: &mut SpaceRegistry, root: &Path) -> PocResult<()> {
    for index in 0..10_000 {
        let id = SpaceId::parse(format!("closed-{index}"))?;
        registry.register_closed(descriptor(
            id,
            SpaceOwner::Team("benchmark-team".to_string()),
            SpaceContext::Project(format!("project-{index}")),
            root.join(format!("closed-{index}")),
        ))?;
    }
    Ok(())
}

fn synthetic_candidates() -> PocResult<Vec<LayeredRecallHit>> {
    let alpha_id = SpaceId::parse("alpha")?;
    let beta_id = SpaceId::parse("beta")?;
    (0..512)
        .map(|index| {
            let denominator = f32::from(u16::try_from(index + 1).expect("candidate index fits"));
            Ok(LayeredRecallHit {
                hit: RecallResult {
                    observation: Observation::new(
                        format!("entity-{index}"),
                        format!("candidate content {index}"),
                        1,
                    ),
                    entity_name: format!("entity-{index}"),
                    entity_type: EntityType::Fact,
                    raw_score: 1.0 / denominator,
                    score: 1.0 / denominator,
                },
                origins: vec![if index % 2 == 0 {
                    alpha_id.clone()
                } else {
                    beta_id.clone()
                }],
                priority: index % 2,
            })
        })
        .collect()
}

fn memory(id: &str, content: String, row_version: u64) -> MemorySnapshot {
    MemorySnapshot {
        logical_id: id.to_string(),
        revision_id: format!("revision-{id}-{row_version}"),
        content,
        row_version,
        deleted: false,
        previous_revision_id: None,
    }
}

fn measure_merge(root: &Path) -> PocResult<MergeMeasurements> {
    let base_memories = (0..10_000)
        .map(|index| memory(&format!("memory-{index:05}"), format!("base-{index}"), 1))
        .collect::<Vec<_>>();
    let source_memories = base_memories
        .iter()
        .enumerate()
        .map(|(index, snapshot)| {
            if index % 2 == 0 {
                memory(&snapshot.logical_id, format!("source-{index}"), 2)
            } else {
                snapshot.clone()
            }
        })
        .collect::<Vec<_>>();
    let target_memories = base_memories
        .iter()
        .enumerate()
        .map(|(index, snapshot)| {
            if index % 2 == 1 {
                memory(&snapshot.logical_id, format!("target-{index}"), 2)
            } else {
                snapshot.clone()
            }
        })
        .collect::<Vec<_>>();
    let base = GraphSnapshot::from_memories(base_memories)?;
    let source = GraphSnapshot::from_memories(source_memories)?;
    let target = GraphSnapshot::from_memories(target_memories)?;
    let plan_stats = measure_n(30, || {
        black_box(plan_merge(&base, &source, &target).expect("plan 10k"));
    });
    let plan = plan_merge(&base, &source, &target)?;

    let base_store = AuditStore::open(&root.join("base"))?;
    let source_store = base_store.snapshot_to(&root.join("source"))?;
    let target_store = base_store.snapshot_to(&root.join("target"))?;
    let ops = (0..1_000)
        .map(|index| DraftOp::Add {
            logical_id: format!("material-{index:04}"),
            content: format!("materialized memory {index}"),
        })
        .collect();
    source_store.submit(
        &ChangeSetDraft::new("material-seed", "benchmark", "material seed", ops),
        SubmitMode::Apply,
    )?;
    let material_plan = plan_merge(
        &GraphSnapshot::read(&base_store)?,
        &GraphSnapshot::read(&source_store)?,
        &GraphSnapshot::read(&target_store)?,
    )?;
    let started = Instant::now();
    black_box(materialize_plan(
        &target_store,
        &material_plan,
        "benchmark",
        None,
    )?);
    let materialize_us = started.elapsed().as_micros();
    Ok(MergeMeasurements {
        plan_10k_memories: plan_stats,
        clean_actions: plan.actions.len(),
        conflicts: plan.conflicts.len(),
        materialize_1k_memories_us: materialize_us,
    })
}
