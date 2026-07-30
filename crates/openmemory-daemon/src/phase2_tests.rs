use std::fs;
use std::process::Command;
use std::str::FromStr;

use openmemory_core::config::Config;
use openmemory_core::space::{
    ActorKind, AuthoritySnapshot, MemoryContext, PathPlatform, PrincipalId, ProfileName, ReadSet,
    SelectionProvenance, SelectionSource, SpaceContext, SpaceGrant, SpaceId, SpaceOwner, SpaceRef,
    SpaceRole, WorkspacePathKey,
};
use rusqlite::params;

use crate::auth::{AuthorizationService, AuthorizedAction};
use crate::policy::{ResolvedProductPolicy, SelectionInput, SelectionInputs};
use crate::product_store::{CatalogSpaceState, ProductStore, ProductStoreError, RootKey};
use crate::services::spaces::{CreateManagedSpace, SpaceService};
use crate::space_manifest::{
    bind_legacy_personal_global, profile_root, ManifestError, MANIFEST_FILE,
};
use crate::space_registry::{RegistryLimits, SpaceLock, SpaceReadiness, SpaceRegistry};

const NOW: i64 = 1_725_000_000;
const HASH: &str = "blake3:0000000000000000000000000000000000000000000000000000000000000000";

fn profile() -> ProfileName {
    ProfileName::new("default").unwrap()
}

fn legacy_binding(
    temp: &tempfile::TempDir,
) -> (ProductStore, crate::space_manifest::LegacyBinding) {
    fs::create_dir_all(temp.path().join("data").join("default")).unwrap();
    let store = ProductStore::open(temp.path()).unwrap();
    let binding =
        bind_legacy_personal_global(&store, &Config::default(), temp.path(), &profile(), 1, NOW)
            .unwrap();
    (store, binding)
}

#[test]
fn product_v1_migrates_to_v4_and_refuses_future_versions() {
    let temp = tempfile::tempdir().unwrap();
    let product_dir = temp.path().join("product");
    fs::create_dir_all(&product_dir).unwrap();
    let path = product_dir.join("product.sqlite");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE product_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO product_meta(key, value) VALUES('schema_version', '1');
         CREATE TABLE daemon_jobs (
             id TEXT PRIMARY KEY, kind_json TEXT NOT NULL, state_json TEXT NOT NULL,
             profile TEXT NOT NULL, created_at_unix_secs INTEGER NOT NULL,
             updated_at_unix_secs INTEGER NOT NULL, job_json TEXT NOT NULL
         );
         CREATE TABLE daemon_events (
             sequence INTEGER PRIMARY KEY, unix_secs INTEGER NOT NULL,
             event_type_json TEXT NOT NULL, job_id TEXT, event_json TEXT NOT NULL
         );
         PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(conn);

    let store = ProductStore::open(temp.path()).unwrap();
    let conn = store.connect().unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT value FROM product_meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "4"
    );
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert!(conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memory_spaces')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap());
    conn.execute(
        "UPDATE product_meta SET value = '5' WHERE key = 'schema_version'",
        [],
    )
    .unwrap();
    conn.pragma_update(None, "user_version", 5_i64).unwrap();
    drop(conn);
    assert!(matches!(
        ProductStore::open(temp.path()),
        Err(ProductStoreError::UnsupportedSchema {
            found: 5,
            supported: 4
        })
    ));

    let mismatch = tempfile::tempdir().unwrap();
    let product_dir = mismatch.path().join("product");
    fs::create_dir_all(&product_dir).unwrap();
    let conn = rusqlite::Connection::open(product_dir.join("product.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE product_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO product_meta(key, value) VALUES('schema_version', '1');
         PRAGMA user_version = 2;",
    )
    .unwrap();
    drop(conn);
    assert!(matches!(
        ProductStore::open(mismatch.path()),
        Err(ProductStoreError::InvalidSchemaVersion(_))
    ));
}

#[test]
fn product_migration_failure_rolls_back_the_entire_step() {
    let temp = tempfile::tempdir().unwrap();
    let product_dir = temp.path().join("product");
    fs::create_dir_all(&product_dir).unwrap();
    let path = product_dir.join("product.sqlite");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE product_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO product_meta(key, value) VALUES('schema_version', '1');
         CREATE TABLE daemon_jobs (
             id TEXT PRIMARY KEY, kind_json TEXT NOT NULL, state_json TEXT NOT NULL,
             profile TEXT NOT NULL, created_at_unix_secs INTEGER NOT NULL,
             updated_at_unix_secs INTEGER NOT NULL, job_json TEXT NOT NULL
         );
         CREATE TABLE daemon_events (
             sequence INTEGER PRIMARY KEY, unix_secs INTEGER NOT NULL,
             event_type_json TEXT NOT NULL, job_id TEXT, event_json TEXT NOT NULL
         );
         -- A malformed pre-existing v2 target makes the v2 index statement
         -- fail after its create-table statement, exercising transaction rollback.
         CREATE TABLE memory_spaces (id TEXT PRIMARY KEY);
         PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(conn);
    assert!(ProductStore::open(temp.path()).is_err());
    let conn = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT value FROM product_meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "1"
    );
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(!conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'local_teams')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap());
}

#[test]
fn legacy_binding_is_restart_stable_across_manifest_repair() {
    let temp = tempfile::tempdir().unwrap();
    let (store, first) = legacy_binding(&temp);
    assert_eq!(first.catalog.state, CatalogSpaceState::Active);
    let id = first.catalog.space.id();
    let root = first.root.clone();
    assert!(root.join(".space-id").is_file());
    assert!(root.join(MANIFEST_FILE).is_file());

    // Simulate a crash after a catalog row became durable but before the
    // manifest/activation completion.  Restart repairs the same identity.
    fs::remove_file(root.join(MANIFEST_FILE)).unwrap();
    let conn = store.connect().unwrap();
    conn.execute(
        "UPDATE memory_spaces SET state = 'creating' WHERE id = ?1",
        params![id.to_string()],
    )
    .unwrap();
    drop(conn);
    let second = bind_legacy_personal_global(
        &store,
        &Config::default(),
        temp.path(),
        &profile(),
        1,
        NOW + 1,
    )
    .unwrap();
    assert_eq!(second.catalog.space.id(), id);
    assert_eq!(second.catalog.root_key, RootKey::LegacyRoot);
    assert_eq!(second.catalog.state, CatalogSpaceState::Active);
    assert!(root.join(MANIFEST_FILE).is_file());
    let conn = store.connect().unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM memory_spaces WHERE root_key = 'legacy-root'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn managed_roots_are_unique_and_symlink_attacks_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let (store, binding) = legacy_binding(&temp);
    let principal = store.ensure_installation_principal(NOW).unwrap();
    let service = SpaceService::new(store.clone(), Config::default(), temp.path().to_path_buf());
    let project_a = store.create_project(&profile(), "A", NOW).unwrap();
    let project_b = store.create_project(&profile(), "B", NOW).unwrap();
    let first = service
        .create(
            CreateManagedSpace {
                space: SpaceRef::new(
                    SpaceId::new(),
                    SpaceOwner::User(principal.clone()),
                    SpaceContext::Project(project_a),
                ),
                profile: profile(),
                domain_count: 1,
            },
            NOW,
        )
        .unwrap();
    let second = service
        .create(
            CreateManagedSpace {
                space: SpaceRef::new(
                    SpaceId::new(),
                    SpaceOwner::User(principal.clone()),
                    SpaceContext::Project(project_b),
                ),
                profile: profile(),
                domain_count: 1,
            },
            NOW,
        )
        .unwrap();
    let profile_root = profile_root(temp.path(), &profile());
    let first_root = crate::space_manifest::root_for(&profile_root, &first.root_key);
    let second_root = crate::space_manifest::root_for(&profile_root, &second.root_key);
    assert_ne!(first_root, second_root);
    assert!(first_root.join(MANIFEST_FILE).is_file());
    assert!(second_root.join(MANIFEST_FILE).is_file());
    assert!(first_root
        .join("store")
        .join(openmemory_graph::MEMORY_DB_FILE)
        .is_file());
    assert!(second_root
        .join("store")
        .join(openmemory_graph::MEMORY_DB_FILE)
        .is_file());
    assert_ne!(binding.catalog.space.id(), first.space.id());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let poisoned = SpaceId::new();
        let poisoned_root = profile_root.join("spaces").join(poisoned.to_string());
        symlink(temp.path(), &poisoned_root).unwrap();
        let result = service.create(
            CreateManagedSpace {
                space: SpaceRef::new(
                    poisoned,
                    SpaceOwner::User(principal),
                    SpaceContext::Project(store.create_project(&profile(), "C", NOW).unwrap()),
                ),
                profile: profile(),
                domain_count: 1,
            },
            NOW,
        );
        assert!(matches!(
            result,
            Err(crate::services::spaces::SpaceServiceError::Manifest(
                ManifestError::UnsafePath
            ))
        ));
    }
}

#[test]
fn workspace_capabilities_and_authority_are_validated_at_the_catalog_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let (store, binding) = legacy_binding(&temp);
    let principal = store.ensure_installation_principal(NOW).unwrap();
    let project = store
        .create_project(&profile(), "workspace project", NOW)
        .unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let workspace =
        WorkspacePathKey::from_canonical_path(&fs::canonicalize(workspace_dir.path()).unwrap(), 1)
            .unwrap();
    store
        .map_workspace(&profile(), &workspace, project, Some("test"), NOW)
        .unwrap();
    assert_eq!(
        store.project_for_workspace(&profile(), &workspace).unwrap(),
        Some(project)
    );

    let authority = AuthoritySnapshot::from_generations(1, &[1, 2, 3]).unwrap();
    let read_set = ReadSet::new(vec![SpaceGrant::new(
        binding.catalog.space.clone(),
        SpaceRole::Maintainer,
        authority,
    )])
    .unwrap();
    let context = MemoryContext::new(
        principal,
        ActorKind::Human,
        profile(),
        None,
        None,
        read_set,
        binding.catalog.space.id(),
        authority,
        SelectionProvenance::new(
            SelectionSource::ProductDefault,
            SelectionSource::ProductDefault,
            SelectionSource::ProductDefault,
            SelectionSource::ProductDefault,
        ),
    )
    .unwrap();
    let bearer = [7_u8; 32];
    store
        .issue_context_capability(&bearer, &context, NOW + 60, NOW)
        .unwrap();
    let restored = store
        .consume_context_capability(&bearer, NOW + 1)
        .unwrap()
        .unwrap();
    assert_eq!(restored.context, context);
    assert_eq!(restored.expires_at_unix_secs, NOW + 60);
    let conn = store.connect().unwrap();
    let stored_hash: String = conn
        .query_row("SELECT token_hash FROM context_capabilities", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_ne!(stored_hash.as_bytes(), bearer.as_slice());
    drop(conn);
    assert!(store.revoke_context_capability(&bearer, NOW + 2).unwrap());
    assert!(store
        .consume_context_capability(&bearer, NOW + 3)
        .unwrap()
        .is_none());
}

#[test]
fn revocation_drains_leases_and_invalidates_team_authority() {
    let temp = tempfile::tempdir().unwrap();
    let (store, _) = legacy_binding(&temp);
    let principal = store.ensure_installation_principal(NOW).unwrap();
    let team = store.create_team(&profile(), "reviewers", NOW).unwrap();
    store
        .grant_team_role(&team, &principal, SpaceRole::Maintainer, None, NOW)
        .unwrap();
    let service = SpaceService::new(store.clone(), Config::default(), temp.path().to_path_buf());
    let team_space = service
        .create(
            CreateManagedSpace {
                space: SpaceRef::new(
                    SpaceId::new(),
                    SpaceOwner::Team(team.clone()),
                    SpaceContext::Global,
                ),
                profile: profile(),
                domain_count: 1,
            },
            NOW,
        )
        .unwrap();
    let authority = AuthorizationService::new(store.clone());
    let lease = authority
        .lease(
            principal.clone(),
            ActorKind::Human,
            &team_space,
            AuthorizedAction::Promotion,
            NOW,
        )
        .unwrap();
    assert!(lease.reauthorize(AuthorizedAction::Promotion, NOW).is_ok());
    assert!(lease.review_authorization(NOW).is_ok());
    drop(lease);
    assert!(authority
        .revoke_team_member(&team, &principal, NOW + 1)
        .unwrap());
    assert!(matches!(
        authority.lease(
            principal,
            ActorKind::Human,
            &team_space,
            AuthorizedAction::Read,
            NOW + 2,
        ),
        Err(crate::auth::AuthorityError::Unauthorized)
    ));
}

#[test]
fn registry_stays_bounded_with_ten_thousand_catalog_rows() {
    let temp = tempfile::tempdir().unwrap();
    let (store, binding) = legacy_binding(&temp);
    let principal = store.ensure_installation_principal(NOW).unwrap();
    let mut conn = store.connect().unwrap();
    let tx = conn.transaction().unwrap();
    {
        let mut insert = tx
            .prepare(
                "INSERT INTO memory_spaces(
                     id, profile, owner_kind, owner_id, context_kind, project_key, root_key,
                     domain_count, manifest_hash, state, catalog_generation,
                     created_at_unix_secs, updated_at_unix_secs
                 ) VALUES(?1, 'default', 'user', ?2, 'project', ?3, ?4, 1, ?5, 'active', 1, ?6, ?6)",
            )
            .unwrap();
        for _ in 0..10_000 {
            let id = SpaceId::new();
            insert
                .execute(params![
                    id.to_string(),
                    principal.as_str(),
                    openmemory_core::space::ProjectId::new().to_string(),
                    RootKey::Space(id).as_str(),
                    HASH,
                    NOW
                ])
                .unwrap();
        }
    }
    tx.commit().unwrap();
    drop(conn);
    assert_eq!(
        store
            .active_spaces_page(&profile(), None, 256)
            .unwrap()
            .len(),
        256
    );

    let mut config = Config::default();
    config.default.jobs = 1;
    let service = SpaceService::new(store.clone(), config.clone(), temp.path().to_path_buf());
    let project_a = store.create_project(&profile(), "bounded A", NOW).unwrap();
    let project_b = store.create_project(&profile(), "bounded B", NOW).unwrap();
    let a = service
        .create(
            CreateManagedSpace {
                space: SpaceRef::new(
                    SpaceId::new(),
                    SpaceOwner::User(principal.clone()),
                    SpaceContext::Project(project_a),
                ),
                profile: profile(),
                domain_count: 1,
            },
            NOW,
        )
        .unwrap();
    let b = service
        .create(
            CreateManagedSpace {
                space: SpaceRef::new(
                    SpaceId::new(),
                    SpaceOwner::User(principal),
                    SpaceContext::Project(project_b),
                ),
                profile: profile(),
                domain_count: 1,
            },
            NOW,
        )
        .unwrap();
    let limits = RegistryLimits {
        max_open_spaces: 1,
        max_open_domains: 1,
        max_graph_connections: 2,
        max_index_handles: 1,
        max_context_engines: 0,
        max_flusher_threads: 0,
    };
    let registry = SpaceRegistry::new(
        store,
        config,
        profile_root(temp.path(), &profile()),
        limits,
        binding.catalog.space.id(),
    )
    .unwrap();
    let lease_a = registry.acquire(a.space.id()).unwrap();
    assert_eq!(lease_a.runtime().readiness(), SpaceReadiness::StoreVerified);
    assert_eq!(lease_a.runtime().store().space_id(), Some(a.space.id()));
    assert_eq!(lease_a.runtime().store().domains(), 1);
    lease_a.runtime().store().status().unwrap();
    drop(lease_a);
    let lease_b = registry.acquire(b.space.id()).unwrap();
    assert_eq!(registry.open_count(), 1);
    assert_eq!(registry.open_domain_count(), 1);
    assert!(matches!(
        registry.close_for_maintenance(b.space.id()),
        Err(crate::space_registry::RegistryError::Leased)
    ));
    drop(lease_b);
    let exclusive = registry.close_for_maintenance(b.space.id()).unwrap();
    assert!(exclusive.lock_path().is_file());
    drop(exclusive);
}

#[test]
fn immutable_policy_preserves_explicit_provenance_and_workspace_key() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let workspace =
        WorkspacePathKey::from_parts(PathPlatform::current(), 1, root.to_str().unwrap()).unwrap();
    let filesystem =
        openmemory_engine::portability::FilesystemCapabilities::inspect(&root).unwrap();
    let policy = ResolvedProductPolicy::resolve(
        "default",
        Some(workspace.clone()),
        SelectionInputs {
            project: SelectionInput::Explicit,
            team: SelectionInput::WorkspaceMapping,
            read_scope: SelectionInput::Explicit,
            write_target: SelectionInput::ProductDefault,
        },
        filesystem,
        RegistryLimits::default(),
    )
    .unwrap();
    assert_eq!(policy.workspace(), Some(&workspace));
    assert_eq!(policy.selection().project(), SelectionSource::Explicit);
    assert_eq!(policy.selection().team(), SelectionSource::WorkspaceMapping);
    assert_eq!(policy.selection().read_scope(), SelectionSource::Explicit);
    assert_eq!(
        policy.selection().write_target(),
        SelectionSource::ProductDefault
    );
    assert!(!policy.filesystem().material_promotion());
}

#[test]
fn scoped_domain_store_rejects_a_catalog_identity_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let first = SpaceId::new();
    let second = SpaceId::new();
    let store = openmemory_engine::partition::DomainStore::open_scoped(
        &Config::default(),
        temp.path(),
        1,
        first,
    )
    .unwrap();
    assert_eq!(store.space_id(), Some(first));
    drop(store);
    assert!(openmemory_engine::partition::DomainStore::open_scoped(
        &Config::default(),
        temp.path(),
        1,
        second,
    )
    .is_err());
}

#[test]
fn cross_process_space_lock_blocks_exclusive_maintenance() {
    const PATH_ENV: &str = "OPENMEMORY_PHASE2_LOCK_PATH";
    const EXPECT_BUSY_ENV: &str = "OPENMEMORY_PHASE2_EXPECT_BUSY";
    if let Ok(path) = std::env::var(PATH_ENV) {
        let result = SpaceLock::exclusive(std::path::Path::new(&path));
        let expect_busy = std::env::var(EXPECT_BUSY_ENV).as_deref() == Ok("1");
        assert_eq!(result.is_err(), expect_busy);
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".space.lock");
    let shared = SpaceLock::shared(&path).unwrap();
    let executable = std::env::current_exe().unwrap();
    let invoke = |expect_busy: bool| {
        Command::new(&executable)
            .args([
                "--exact",
                "phase2_tests::cross_process_space_lock_blocks_exclusive_maintenance",
                "--nocapture",
            ])
            .env(PATH_ENV, &path)
            .env(EXPECT_BUSY_ENV, if expect_busy { "1" } else { "0" })
            .status()
            .unwrap()
    };
    assert!(invoke(true).success());
    drop(shared);
    assert!(invoke(false).success());
}

#[test]
fn opaque_principal_inputs_remain_validated() {
    assert!(PrincipalId::from_str("\0invalid").is_err());
}
