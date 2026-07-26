use openmemory_core::config::Config;
use openmemory_graph::{EntityType, ObservationInput, RecallFilters, SearchMode};
use openmemory_memory_model_poc::spaces::{
    descriptor, MemorySpace, ReadSet, SpaceContext, SpaceId, SpaceOwner, SpaceRegistry,
    MAX_READ_SET,
};

fn filters() -> RecallFilters {
    RecallFilters {
        mode: Some(SearchMode::KeywordOnly),
        spreading_activation: false,
        record_access: false,
        ..RecallFilters::default()
    }
}

fn open_space(root: &std::path::Path, id: &str) -> MemorySpace {
    open_space_with_config(root, id, &Config::default())
}

fn open_space_with_config(root: &std::path::Path, id: &str, config: &Config) -> MemorySpace {
    let id = SpaceId::parse(id).expect("space id");
    let project_id = id.as_str().to_string();
    MemorySpace::open(
        config,
        descriptor(
            id,
            SpaceOwner::User("alice".to_string()),
            SpaceContext::Project(project_id),
            root,
        ),
    )
    .expect("open space")
}

#[test]
fn isolation_composes_with_production_domain_partitioning() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mut config = Config::default();
    config.engine.domains = 4;
    let alpha = open_space_with_config(&directory.path().join("alpha-4d"), "alpha4", &config);
    let beta = open_space_with_config(&directory.path().join("beta-4d"), "beta4", &config);
    assert_eq!(alpha.store().domains(), 4);
    assert_eq!(beta.store().domains(), 4);
    for index in 0..64 {
        alpha
            .store()
            .remember(
                &format!("Alpha entity {index}"),
                EntityType::Fact,
                &[ObservationInput::new(format!(
                    "alphapartition marker {index}"
                ))],
                &[],
                "test",
            )
            .expect("seed partitioned alpha");
        beta.store()
            .remember(
                &format!("Beta entity {index}"),
                EntityType::Fact,
                &[ObservationInput::new(format!(
                    "betapartition marker {index}"
                ))],
                &[],
                "test",
            )
            .expect("seed partitioned beta");
    }
    let mut registry = SpaceRegistry::new();
    registry.insert_open(alpha).expect("register alpha");
    registry.insert_open(beta).expect("register beta");
    let alpha_only = ReadSet::new(vec![SpaceId::parse("alpha4").expect("id")]).expect("read set");
    let overlay = ReadSet::new(vec![
        SpaceId::parse("alpha4").expect("id"),
        SpaceId::parse("beta4").expect("id"),
    ])
    .expect("overlay");
    assert!(registry
        .recall_parallel(&alpha_only, "betapartition", 10, &filters())
        .expect("isolated recall")
        .is_empty());
    assert!(!registry
        .recall_parallel(&overlay, "betapartition", 10, &filters())
        .expect("overlay recall")
        .is_empty());
}

#[test]
fn project_roots_are_physically_isolated_until_explicit_overlay() {
    let directory = tempfile::tempdir().expect("tempdir");
    let alpha = open_space(&directory.path().join("alpha"), "alpha");
    alpha
        .store()
        .remember(
            "Project Alpha",
            EntityType::Project,
            &[ObservationInput::new("uses ferritealpha for persistence")],
            &[],
            "test",
        )
        .expect("seed alpha");
    let beta = open_space(&directory.path().join("beta"), "beta");
    beta.store()
        .remember(
            "Project Beta",
            EntityType::Project,
            &[ObservationInput::new("uses cobaltbeta for persistence")],
            &[],
            "test",
        )
        .expect("seed beta");

    let mut registry = SpaceRegistry::new();
    registry.insert_open(alpha).expect("register alpha");
    registry.insert_open(beta).expect("register beta");
    let alpha_only = ReadSet::new(vec![SpaceId::parse("alpha").expect("id")]).expect("read set");
    let overlay = ReadSet::new(vec![
        SpaceId::parse("alpha").expect("id"),
        SpaceId::parse("beta").expect("id"),
    ])
    .expect("overlay");

    assert!(registry
        .recall_parallel(&alpha_only, "cobaltbeta", 10, &filters())
        .expect("alpha recall")
        .is_empty());
    let overlaid = registry
        .recall_parallel(&overlay, "cobaltbeta", 10, &filters())
        .expect("overlay recall");
    assert_eq!(overlaid.len(), 1);
    assert_eq!(overlaid[0].origins[0].as_str(), "beta");
}

#[test]
fn duplicate_results_are_deterministic_and_retain_provenance() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mut registry = SpaceRegistry::new();
    for id in ["private", "team"] {
        let space = open_space(&directory.path().join(id), id);
        space
            .store()
            .remember(
                "Convention",
                EntityType::Fact,
                &[ObservationInput::new("run cargo clippy before merging")],
                &[],
                "test",
            )
            .expect("seed duplicate");
        registry.insert_open(space).expect("register");
    }
    let read_set = ReadSet::new(vec![
        SpaceId::parse("private").expect("id"),
        SpaceId::parse("team").expect("id"),
    ])
    .expect("read set");
    let sequential = registry
        .recall_sequential(&read_set, "clippy", 10, &filters())
        .expect("sequential");
    let parallel = registry
        .recall_parallel(&read_set, "clippy", 10, &filters())
        .expect("parallel");

    assert_eq!(sequential.len(), 1);
    assert_eq!(parallel.len(), 1);
    assert_eq!(sequential[0].origins, parallel[0].origins);
    assert_eq!(
        parallel[0]
            .origins
            .iter()
            .map(SpaceId::as_str)
            .collect::<Vec<_>>(),
        vec!["private", "team"]
    );
}

#[test]
fn read_set_is_bounded_unique_and_path_safe() {
    let ids = (0..=MAX_READ_SET)
        .map(|index| SpaceId::parse(format!("space-{index}")).expect("id"))
        .collect();
    assert!(ReadSet::new(ids).is_err());
    let duplicate = SpaceId::parse("same").expect("id");
    assert!(ReadSet::new(vec![duplicate.clone(), duplicate]).is_err());
    assert!(SpaceId::parse("../escape").is_err());
    assert!(SpaceId::parse("").is_err());
}

#[test]
fn catalog_size_does_not_expand_the_active_read_set() {
    let directory = tempfile::tempdir().expect("tempdir");
    let active = open_space(&directory.path().join("active"), "active");
    active
        .store()
        .remember(
            "Active",
            EntityType::Fact,
            &[ObservationInput::new("boundedreadset marker")],
            &[],
            "test",
        )
        .expect("seed");
    let mut registry = SpaceRegistry::new();
    registry.insert_open(active).expect("active");
    for index in 0..10_000 {
        let id = SpaceId::parse(format!("closed-{index}")).expect("id");
        registry
            .register_closed(descriptor(
                id,
                SpaceOwner::Team("large-team".to_string()),
                SpaceContext::Project(format!("project-{index}")),
                directory.path().join(format!("not-opened-{index}")),
            ))
            .expect("register closed");
    }
    assert_eq!(registry.registered_count(), 10_001);
    let active_only = ReadSet::new(vec![SpaceId::parse("active").expect("id")]).expect("read set");
    assert_eq!(
        registry
            .recall_parallel(&active_only, "boundedreadset", 10, &filters())
            .expect("recall")
            .len(),
        1
    );
}
