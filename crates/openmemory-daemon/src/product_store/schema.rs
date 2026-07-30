//! Ordered product control-plane migrations.
//!
//! Product state is deliberately kept separate from graph state.  Every
//! migration owns one SQLite transaction and advances both the explicit
//! metadata version and SQLite's `user_version` together, so an interrupted
//! upgrade never exposes a partially applied catalog.

use openmemory_core::migrations::Migrator;
use rusqlite::{Connection, OptionalExtension};

use super::ProductStoreError;

pub(super) const PRODUCT_SCHEMA_VERSION: i64 = 4;
const SCHEMA_VERSION_KEY: &str = "schema_version";

pub(super) fn migrate(conn: &mut Connection) -> Result<(), ProductStoreError> {
    let current = read_version(conn)?;
    if current > PRODUCT_SCHEMA_VERSION {
        return Err(ProductStoreError::UnsupportedSchema {
            found: current,
            supported: PRODUCT_SCHEMA_VERSION,
        });
    }
    Migrator::new(conn, "product_meta")
        .apply_with_user_version(
            PRODUCT_SCHEMA_VERSION as u32,
            &[(1, V1_SQL), (2, V2_SQL), (3, V3_SQL), (4, V4_SQL)],
        )
        .map_err(|error| ProductStoreError::InvalidSchemaVersion(error.to_string()))
}

fn read_version(conn: &Connection) -> Result<i64, ProductStoreError> {
    let has_meta = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'product_meta'
         )",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !has_meta {
        return Ok(0);
    }

    let version = conn
        .query_row(
            "SELECT value FROM product_meta WHERE key = ?1",
            [SCHEMA_VERSION_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(version) = version else {
        return Ok(0);
    };
    let version = version
        .parse::<i64>()
        .map_err(|_| ProductStoreError::InvalidSchemaVersion(version))?;
    let pragma_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if pragma_version != 0 && pragma_version != version {
        return Err(ProductStoreError::InvalidSchemaVersion(format!(
            "metadata version {version} disagrees with user_version {pragma_version}",
        )));
    }
    Ok(version)
}

const V1_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS product_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS daemon_jobs (
    id TEXT PRIMARY KEY,
    kind_json TEXT NOT NULL,
    state_json TEXT NOT NULL,
    profile TEXT NOT NULL,
    created_at_unix_secs INTEGER NOT NULL,
    updated_at_unix_secs INTEGER NOT NULL,
    job_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_daemon_jobs_created
    ON daemon_jobs(created_at_unix_secs, id);
CREATE INDEX IF NOT EXISTS idx_daemon_jobs_state
    ON daemon_jobs(state_json);
CREATE TABLE IF NOT EXISTS daemon_events (
    sequence INTEGER PRIMARY KEY,
    unix_secs INTEGER NOT NULL,
    event_type_json TEXT NOT NULL,
    job_id TEXT,
    event_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_daemon_events_job
    ON daemon_events(job_id, sequence);
"#;

const V2_SQL: &str = r#"
CREATE TABLE memory_spaces (
    id TEXT PRIMARY KEY,
    profile TEXT NOT NULL,
    owner_kind TEXT NOT NULL CHECK(owner_kind IN ('user', 'team')),
    owner_id TEXT NOT NULL,
    context_kind TEXT NOT NULL CHECK(context_kind IN ('global', 'project')),
    project_key TEXT NOT NULL,
    root_key TEXT NOT NULL UNIQUE,
    domain_count INTEGER NOT NULL CHECK(domain_count BETWEEN 1 AND 64),
    manifest_hash TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('creating', 'active', 'closed', 'deleting', 'error')),
    catalog_generation INTEGER NOT NULL CHECK(catalog_generation >= 0),
    created_at_unix_secs INTEGER NOT NULL,
    updated_at_unix_secs INTEGER NOT NULL,
    UNIQUE(profile, owner_kind, owner_id, context_kind, project_key)
);
CREATE INDEX idx_memory_spaces_profile_state
    ON memory_spaces(profile, state, created_at_unix_secs, id);
CREATE INDEX idx_memory_spaces_owner
    ON memory_spaces(profile, owner_kind, owner_id, state);

CREATE TABLE local_principals (
    id TEXT PRIMARY KEY,
    display_label TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('active', 'disabled')),
    generation INTEGER NOT NULL CHECK(generation >= 0),
    created_at_unix_secs INTEGER NOT NULL,
    updated_at_unix_secs INTEGER NOT NULL
);
CREATE INDEX idx_local_principals_state ON local_principals(state, id);

CREATE TABLE local_teams (
    id TEXT PRIMARY KEY,
    profile TEXT NOT NULL,
    label TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('active', 'archived')),
    authority_generation INTEGER NOT NULL CHECK(authority_generation >= 0),
    created_at_unix_secs INTEGER NOT NULL,
    updated_at_unix_secs INTEGER NOT NULL
);
CREATE INDEX idx_local_teams_profile_state ON local_teams(profile, state, id);

CREATE TABLE team_memberships (
    team_id TEXT NOT NULL REFERENCES local_teams(id) ON DELETE RESTRICT,
    principal_id TEXT NOT NULL REFERENCES local_principals(id) ON DELETE RESTRICT,
    role TEXT NOT NULL CHECK(role IN ('reader', 'contributor', 'reviewer', 'maintainer')),
    authority_generation INTEGER NOT NULL CHECK(authority_generation >= 0),
    expires_at_unix_secs INTEGER,
    created_at_unix_secs INTEGER NOT NULL,
    updated_at_unix_secs INTEGER NOT NULL,
    PRIMARY KEY(team_id, principal_id)
);
CREATE INDEX idx_team_memberships_principal
    ON team_memberships(principal_id, team_id, expires_at_unix_secs);

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    profile TEXT NOT NULL,
    label TEXT NOT NULL,
    created_at_unix_secs INTEGER NOT NULL,
    updated_at_unix_secs INTEGER NOT NULL
);
CREATE INDEX idx_projects_profile_created ON projects(profile, created_at_unix_secs, id);

CREATE TABLE workspace_projects (
    workspace_id TEXT PRIMARY KEY,
    profile TEXT NOT NULL,
    path_platform TEXT NOT NULL CHECK(path_platform IN ('linux', 'mac_os', 'windows')),
    normalizer_version INTEGER NOT NULL CHECK(normalizer_version > 0),
    canonical_path TEXT NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    vcs_fingerprint TEXT,
    state TEXT NOT NULL CHECK(state IN ('active', 'moved', 'detached')),
    created_at_unix_secs INTEGER NOT NULL,
    updated_at_unix_secs INTEGER NOT NULL,
    UNIQUE(profile, canonical_path)
);
CREATE INDEX idx_workspace_projects_profile_state
    ON workspace_projects(profile, state, project_id);

CREATE TABLE context_capabilities (
    token_hash TEXT PRIMARY KEY,
    principal_id TEXT NOT NULL REFERENCES local_principals(id) ON DELETE RESTRICT,
    actor_kind TEXT NOT NULL CHECK(actor_kind IN ('human', 'agent', 'system')),
    profile TEXT NOT NULL,
    context_json TEXT NOT NULL,
    authority_digest TEXT NOT NULL,
    expires_at_unix_secs INTEGER NOT NULL,
    revoked_at_unix_secs INTEGER,
    created_at_unix_secs INTEGER NOT NULL
);
CREATE INDEX idx_context_capabilities_lookup
    ON context_capabilities(principal_id, profile, expires_at_unix_secs);
"#;

const V3_SQL: &str = r#"
CREATE TABLE identity_candidates (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    pair_key TEXT NOT NULL,
    left_revision_id TEXT NOT NULL,
    right_revision_id TEXT NOT NULL,
    policy_generation INTEGER NOT NULL,
    ontology_generation INTEGER NOT NULL,
    resolver_generation INTEGER NOT NULL,
    packet_hash TEXT NOT NULL,
    packet_json TEXT NOT NULL,
    state TEXT NOT NULL,
    review_head TEXT,
    UNIQUE(job_id, pair_key)
);
CREATE TABLE identity_agent_proposals (
    id TEXT PRIMARY KEY,
    candidate_id TEXT NOT NULL REFERENCES identity_candidates(id) ON DELETE CASCADE,
    revision_id TEXT NOT NULL,
    packet_hash TEXT NOT NULL,
    provenance_json TEXT NOT NULL,
    proposal_json TEXT NOT NULL,
    valid_until_unix_secs INTEGER
);
CREATE TABLE identity_decision_events (
    id TEXT PRIMARY KEY,
    candidate_id TEXT NOT NULL REFERENCES identity_candidates(id) ON DELETE RESTRICT,
    packet_hash TEXT NOT NULL,
    supersedes_id TEXT,
    actor_principal_id TEXT NOT NULL,
    rationale TEXT NOT NULL,
    created_at_unix_secs INTEGER NOT NULL
);
CREATE TABLE merge_jobs (
    id TEXT PRIMARY KEY,
    source_space_id TEXT NOT NULL,
    target_space_id TEXT NOT NULL,
    state TEXT NOT NULL,
    source_manifest_key TEXT,
    target_manifest_key TEXT,
    target_version TEXT,
    action_stream_hash TEXT,
    plan_hash TEXT,
    predicted_result_hash TEXT,
    confirmation_hash TEXT,
    confirmation_expires_at_unix_secs INTEGER,
    report_json TEXT,
    error_json TEXT,
    created_at_unix_secs INTEGER NOT NULL,
    updated_at_unix_secs INTEGER NOT NULL
);
CREATE TABLE merge_resolutions (
    job_id TEXT NOT NULL REFERENCES merge_jobs(id) ON DELETE CASCADE,
    candidate_id TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    PRIMARY KEY(job_id, candidate_id)
);
CREATE TABLE space_lineages (
    id INTEGER PRIMARY KEY,
    source_space_id TEXT NOT NULL,
    target_space_id TEXT NOT NULL,
    merge_job_id TEXT NOT NULL,
    source_hash TEXT NOT NULL,
    prior_target_hash TEXT NOT NULL,
    result_hash TEXT NOT NULL,
    manifest_key TEXT NOT NULL,
    created_at_unix_secs INTEGER NOT NULL
);
"#;

const V4_SQL: &str = r#"
CREATE TABLE maintenance_tasks (
    id TEXT PRIMARY KEY,
    profile TEXT NOT NULL,
    space_id TEXT,
    kind TEXT NOT NULL,
    state TEXT NOT NULL,
    cursor_json TEXT NOT NULL,
    counters_json TEXT NOT NULL,
    error_json TEXT,
    created_at_unix_secs INTEGER NOT NULL,
    updated_at_unix_secs INTEGER NOT NULL
);
CREATE INDEX idx_maintenance_tasks_profile_state
    ON maintenance_tasks(profile, state, created_at_unix_secs, id);
"#;
