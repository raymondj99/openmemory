#![allow(dead_code)]

use openmemory_core::space::{RevisionId, SnapshotId, SpaceId};
use openmemory_merge::discovery::{CandidateIndex, DEFAULT_CANDIDATE_CAP};
use openmemory_merge::evidence::IdentityPolicy;
use openmemory_merge::hash::SemanticHash;
use openmemory_merge::model::{
    DomainGeneration, EntityRecord, LogicalId, MemorySnapshot, ObjectAddress, RelationRecord,
    SpaceVersion,
};
use openmemory_merge::planner::{
    plan_merge, ActionSink, MergeAction, MergePlan, MergePolicy, PlanPrelude, PlanningReceipts,
};
use openmemory_merge::MergeResult;

pub struct ScaleCase {
    target: MemorySnapshot,
    source: MemorySnapshot,
    receipts: PlanningReceipts,
    policy: MergePolicy,
}

impl ScaleCase {
    pub fn new(source_entities: usize) -> Self {
        assert!(source_entities > 1);
        let target_space = uuid7(700_001).parse::<SpaceId>().unwrap();
        let source_space = uuid7(700_002).parse::<SpaceId>().unwrap();
        let target_snapshot = uuid7(700_003).parse::<SnapshotId>().unwrap();
        let source_snapshot = uuid7(700_004).parse::<SnapshotId>().unwrap();
        let target = MemorySnapshot::new(
            target_snapshot,
            target_space,
            version(1),
            vec![EntityRecord::new(
                ObjectAddress::new(target_space, LogicalId::new("target-root").unwrap()),
                uuid7(700_005).parse::<RevisionId>().unwrap(),
                "target root",
                Some("concept"),
            )
            .unwrap()],
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let source_records = (0..source_entities)
            .map(|index| {
                EntityRecord::new(
                    ObjectAddress::new(
                        source_space,
                        LogicalId::new(format!("source-{index:06}")).unwrap(),
                    ),
                    uuid7(800_000 + u64::try_from(index).unwrap())
                        .parse::<RevisionId>()
                        .unwrap(),
                    format!("source label {index:06}"),
                    Some("concept"),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let source_relations = (0..source_entities)
            .map(|index| {
                let next = (index + 1) % source_entities;
                RelationRecord::new(
                    ObjectAddress::new(
                        source_space,
                        LogicalId::new(format!("relation-{index:06}")).unwrap(),
                    ),
                    ObjectAddress::new(
                        source_space,
                        LogicalId::new(format!("source-{index:06}")).unwrap(),
                    ),
                    "precedes",
                    ObjectAddress::new(
                        source_space,
                        LogicalId::new(format!("source-{next:06}")).unwrap(),
                    ),
                    uuid7(1_000_000 + u64::try_from(index).unwrap())
                        .parse::<RevisionId>()
                        .unwrap(),
                )
                .unwrap()
            })
            .collect();
        let source = MemorySnapshot::new(
            source_snapshot,
            source_space,
            version(2),
            source_records,
            Vec::new(),
            source_relations,
        )
        .unwrap();
        let identity_policy = IdentityPolicy::new(1, 1).unwrap();
        let index = CandidateIndex::build(
            1,
            target_snapshot,
            identity_policy,
            target.entities().iter().cloned(),
        )
        .unwrap();
        let discoveries = source
            .entities()
            .iter()
            .map(|entity| {
                index
                    .discover(entity, source_snapshot, DEFAULT_CANDIDATE_CAP)
                    .unwrap()
            })
            .collect();
        Self {
            target,
            source,
            receipts: PlanningReceipts::new(discoveries, Vec::new(), Vec::new()),
            policy: MergePolicy::new(1, 1).unwrap(),
        }
    }

    pub fn run(&mut self) -> MergePlan {
        let mut sink = DiscardSink::default();
        plan_merge(
            &mut self.target,
            &mut self.source,
            &self.receipts,
            &self.policy,
            &mut sink,
        )
        .unwrap()
    }

    pub fn canonical_snapshot_bytes(&self) -> usize {
        self.target
            .canonical_bytes()
            .saturating_add(self.source.canonical_bytes())
    }
}

#[derive(Default)]
struct DiscardSink {
    begun: bool,
    finished: bool,
}

impl ActionSink for DiscardSink {
    fn begin(&mut self, _prelude: &PlanPrelude) -> MergeResult<()> {
        self.begun = true;
        Ok(())
    }

    fn emit(&mut self, _action: &MergeAction) -> MergeResult<()> {
        assert!(self.begun && !self.finished);
        Ok(())
    }

    fn finish(&mut self, _plan: &MergePlan) -> MergeResult<()> {
        assert!(self.begun && !self.finished);
        self.finished = true;
        Ok(())
    }

    fn abort(&mut self) {
        self.finished = false;
    }
}

fn version(value: u8) -> SpaceVersion {
    SpaceVersion::new(
        vec![DomainGeneration::new(
            0,
            u64::from(value),
            u64::from(value),
            u64::from(value),
        )],
        SemanticHash::from_bytes([value; 32]),
    )
    .unwrap()
}

fn uuid7(sequence: u64) -> String {
    format!("018f6b7a-4d3c-7abc-8def-{sequence:012x}")
}
