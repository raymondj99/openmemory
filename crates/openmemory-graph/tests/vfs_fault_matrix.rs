//! SQLite VFS fault matrix for the production storage path.
//!
//! Phase 3 of `plan/16-production-memory-spaces/09-delivery-sequence.md`
//! requires "transaction, concurrency, abort, and SQLite VFS fault tests".
//! The faults here are injected *below* SQLite by a controllable pass-through
//! VFS registered as the process default (see `fault_vfs`), so everything
//! above it — `MemoryStore::open`, the audited changeset transaction,
//! `wal_checkpoint`, `repair_index`, `recall` — is the real production code
//! with no injection seam of its own.
//!
//! ## What each test asserts
//!
//! The assertion is never "it errors". For each fault the store must keep
//! its invariants:
//!
//! - **No torn canonical state.** Every observation that names a current
//!   revision has that revision; every revision names a changeset that
//!   exists; `PRAGMA integrity_check` and `PRAGMA foreign_key_check` are
//!   clean.
//! - **No partially-applied changeset.** The changeset under fault is either
//!   entirely present with all of its effects, or entirely absent. The tests
//!   check the specific content they submitted, not just row counts.
//! - **Derived indexes consistent or fail-closed.** A pending index outbox
//!   row implies `index_repair_required`, and while that flag is set `recall`
//!   returns `MemoryError::IndexRepairRequired` rather than partial results.
//! - **Reopen recovers or fails cleanly.** Every test reopens the store
//!   through `MemoryStore::open` after the fault and re-checks the above.
//!
//! ## Never the live store
//!
//! Every test uses a fresh `tempfile::tempdir()`. `MemoryStore::open` runs
//! migrations, so pointing any of this at `~/.openmemory/data/default` would
//! mutate real user data.
//!
//! ## This file is not feature-gated, deliberately
//!
//! A `#![cfg(feature = "...")]` integration test compiles to zero tests when
//! the feature is absent, and a suite of zero tests passes silently. Nothing
//! here is gated: it builds and runs under default features,
//! `--all-features`, and `--no-default-features`.
//! `harness_shim_is_default_vfs_and_sees_production_io` and
//! `harness_counts_matches_separately_from_fires` fail loudly if the
//! injection machinery ever stops working, and every fault test calls
//! `FaultHandle::assert_fired`.

mod fault_vfs;

use std::path::Path;

use fault_vfs::{Effect, FaultSpec, FileKind, OpKind};
use openmemory_core::config::Config;
use openmemory_core::space::{ActorKind, AuthoritySnapshot, SpaceId};
use openmemory_graph::{
    ChangeOperation, ChangeSetDraft, EntityType, MemoryError, MemoryStore, ObservationInput,
    RecallFilters, SearchMode, SubmitMode, MEMORY_DB_FILE,
};
use rusqlite::ffi::{SQLITE_CANTOPEN, SQLITE_FULL, SQLITE_IOERR_FSYNC, SQLITE_IOERR_WRITE};
use rusqlite::{Connection, OpenFlags};

/// A single reader slot keeps *which* connection performs a given read
/// deterministic, so an `nth`-match rule stays reproducible.
fn config() -> Config {
    let mut config = Config::default();
    config.default.jobs = 1;
    config
}

fn authority() -> AuthoritySnapshot {
    AuthoritySnapshot::from_generations(1, &[1]).unwrap()
}

/// Submit one audited `Remember` through the production changeset path.
fn remember(
    store: &MemoryStore,
    space: SpaceId,
    key: &str,
    entity: &str,
    content: &str,
) -> Result<(), MemoryError> {
    let draft = ChangeSetDraft::new(
        key,
        space,
        ActorKind::Agent,
        "vfs-fault-suite",
        authority(),
        "vfs fault matrix",
        "test",
        vec![ChangeOperation::Remember {
            entity_name: entity.to_owned(),
            entity_type: EntityType::Fact,
            observations: vec![ObservationInput::new(content)],
            relations: vec![],
        }],
    )?;
    store.submit_changeset(draft, SubmitMode::ApplyImmediately)?;
    Ok(())
}

/// Build a populated store, checkpoint it so the baseline lives in the main
/// database file rather than the WAL, and close it. Returns the space the
/// store is bound to.
fn seed(dir: &Path) -> SpaceId {
    fault_vfs::install();
    fault_vfs::watch(dir);
    let space = SpaceId::new();
    let store = MemoryStore::open(&config(), dir).unwrap();
    store.bind_space_id(space).unwrap();
    remember(&store, space, "seed-1", "Baseline", "baseline fact one").unwrap();
    remember(&store, space, "seed-2", "Baseline", "baseline fact two").unwrap();
    let report = store.wal_checkpoint().unwrap();
    assert!(report.complete, "seed checkpoint must fully drain the WAL");
    drop(store);
    space
}

/// Canonical content as the production read API reports it: sorted
/// `entity|content` pairs over live observations.
fn canonical(store: &MemoryStore) -> Vec<String> {
    let mut rows = Vec::new();
    for entity in store.list_entities(None, 10_000, 0).unwrap() {
        for observation in store.get_entity_observations(&entity.entity.id).unwrap() {
            rows.push(format!("{}|{}", entity.entity.name, observation.content));
        }
    }
    rows.sort();
    rows
}

fn baseline() -> Vec<String> {
    vec![
        "Baseline|baseline fact one".to_owned(),
        "Baseline|baseline fact two".to_owned(),
    ]
}

fn read_only(dir: &Path) -> Connection {
    Connection::open_with_flags(
        dir.join(MEMORY_DB_FILE),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("the database file must still be openable")
}

/// Structural invariants that must hold after any fault, checked against the
/// file directly so a broken store cannot hide them behind its own API. This
/// opens read-only and never migrates.
fn assert_structural_invariants(dir: &Path, context: &str) {
    let conn = read_only(dir);

    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok", "{context}: integrity_check");

    let fk_violations = conn
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query_map([], |_| Ok(()))
        .unwrap()
        .count();
    assert_eq!(fk_violations, 0, "{context}: foreign_key_check");

    let counted = |sql: &str| -> i64 { conn.query_row(sql, [], |row| row.get(0)).unwrap() };

    assert_eq!(
        counted(
            "SELECT COUNT(*) FROM observations o
             WHERE o.current_revision_id IS NOT NULL
               AND NOT EXISTS(
                   SELECT 1 FROM observation_revisions r
                   WHERE r.revision_id = o.current_revision_id)"
        ),
        0,
        "{context}: an observation head revision is missing (torn canonical state)"
    );

    assert_eq!(
        counted(
            "SELECT COUNT(*) FROM observation_revisions r
             WHERE NOT EXISTS(SELECT 1 FROM change_sets c WHERE c.id = r.changeset_id)"
        ),
        0,
        "{context}: every revision must name a surviving changeset"
    );

    assert_eq!(
        counted(
            "SELECT COUNT(*) FROM change_sets c
             WHERE c.state = 'applied'
               AND NOT EXISTS(SELECT 1 FROM change_events e WHERE e.changeset_id = c.id)"
        ),
        0,
        "{context}: an applied changeset without events is a partial apply"
    );

    let pending = counted("SELECT COUNT(*) FROM index_outbox WHERE applied_at IS NULL");
    let repair_required = counted("SELECT index_repair_required FROM domain_state WHERE id = 1");
    assert!(
        pending == 0 || repair_required == 1,
        "{context}: {pending} unapplied outbox rows without index_repair_required; \
         the derived index is neither consistent nor fail-closed"
    );
}

/// The store's derived-index state must agree with what `recall` does: stale
/// means fail closed, current means serve.
fn assert_recall_matches_index_state(store: &MemoryStore, dir: &Path, context: &str) {
    let repair_required: i64 = read_only(dir)
        .query_row(
            "SELECT index_repair_required FROM domain_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut filters = RecallFilters::new();
    filters.mode = Some(SearchMode::KeywordOnly);
    let outcome = store.recall("baseline", 5, &filters);
    if repair_required == 1 {
        assert!(
            matches!(outcome, Err(MemoryError::IndexRepairRequired)),
            "{context}: index_repair_required is set but recall did not fail closed"
        );
    } else {
        assert!(
            !matches!(outcome, Err(MemoryError::IndexRepairRequired)),
            "{context}: recall reports a stale index that domain_state does not"
        );
    }
}

/// Does a changeset with this idempotency key exist, and in what state?
fn changeset_state(dir: &Path, key: &str) -> Option<String> {
    read_only(dir)
        .query_row(
            "SELECT state FROM change_sets WHERE idempotency_key = ?1",
            [key],
            |row| row.get::<_, String>(0),
        )
        .ok()
}

/// Reopen through the production path and assert the store is either fully
/// usable or reports a clean typed failure. Returns the canonical content
/// when the reopen succeeded.
fn reopen_and_check(dir: &Path, context: &str) -> Option<Vec<String>> {
    match MemoryStore::open(&config(), dir) {
        Ok(store) => {
            let rows = canonical(&store);
            assert_recall_matches_index_state(&store, dir, context);
            drop(store);
            assert_structural_invariants(dir, context);
            Some(rows)
        }
        Err(error) => {
            // A clean typed failure is an acceptable outcome; a panic, a hang,
            // or a silently truncated store is not. The file must still be
            // structurally sound.
            assert!(
                !format!("{error}").is_empty(),
                "{context}: reopen failed without a message"
            );
            assert_structural_invariants(dir, context);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Harness self-tests. These exist because a fault-injection suite that stops
// injecting passes everything and proves nothing.
// ---------------------------------------------------------------------------

#[test]
fn harness_shim_is_default_vfs_and_sees_production_io() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    assert!(
        fault_vfs::is_default_vfs(),
        "the shim is not the default VFS; every fault test below would be a no-op"
    );

    // The production store must actually route its main-database and WAL
    // traffic through the shim, otherwise the injection points below are
    // never reached.
    for (kind, op) in [
        (FileKind::MainDb, OpKind::Open),
        (FileKind::MainDb, OpKind::Read),
        (FileKind::MainDb, OpKind::Write),
        (FileKind::MainDb, OpKind::Sync),
        (FileKind::Wal, OpKind::Open),
        (FileKind::Wal, OpKind::Write),
    ] {
        let count = fault_vfs::observed(dir.path(), MEMORY_DB_FILE, kind, op);
        assert!(
            count > 0,
            "the shim intercepted no {kind:?}/{op:?} for {MEMORY_DB_FILE}; \
             faults on that entry point could not fire"
        );
    }
}

#[test]
fn harness_counts_matches_separately_from_fires() {
    let dir = tempfile::tempdir().unwrap();
    let space = seed(dir.path());
    let store = MemoryStore::open(&config(), dir.path()).unwrap();

    // A rule whose nth can never be reached: its predicate still matches, so
    // `matched` climbs while `fired` stays at zero. This is what makes
    // `assert_fired` in every other test a real check rather than a constant.
    let unreachable = fault_vfs::arm(
        "unreachable-nth",
        FaultSpec {
            nth: u64::MAX,
            ..FaultSpec::once(
                dir.path(),
                MEMORY_DB_FILE,
                FileKind::Wal,
                OpKind::Write,
                Effect::Fail(SQLITE_FULL),
            )
        },
    );

    remember(&store, space, "unreachable", "Baseline", "still committed").unwrap();

    assert!(
        unreachable.matched() > 0,
        "the rule predicate never matched, so a zero fire count would be ambiguous"
    );
    assert_eq!(
        unreachable.fired(),
        0,
        "a rule that cannot reach its nth match must not fire"
    );

    drop(store);
    assert_structural_invariants(dir.path(), "unreachable rule");
}

// ---------------------------------------------------------------------------
// The fault matrix.
// ---------------------------------------------------------------------------

/// `SQLITE_FULL` on the first WAL write of an audited commit.
#[test]
fn sqlite_full_on_wal_write_aborts_the_audited_commit() {
    let dir = tempfile::tempdir().unwrap();
    let space = seed(dir.path());
    let store = MemoryStore::open(&config(), dir.path()).unwrap();

    let fault = fault_vfs::arm(
        "SQLITE_FULL on WAL write",
        FaultSpec::once(
            dir.path(),
            MEMORY_DB_FILE,
            FileKind::Wal,
            OpKind::Write,
            Effect::Fail(SQLITE_FULL),
        ),
    );

    let result = remember(&store, space, "full-1", "DiskFull", "must not be visible");
    fault.assert_fired();
    assert!(
        result.is_err(),
        "a full disk must not be reported as a successful audited commit"
    );
    fault.disarm();

    // Same open store: canonical state is the pre-fault baseline.
    assert_eq!(canonical(&store), baseline(), "state after SQLITE_FULL");
    drop(store);

    assert_eq!(
        changeset_state(dir.path(), "full-1"),
        None,
        "the aborted changeset must leave no row at all"
    );
    assert_structural_invariants(dir.path(), "after SQLITE_FULL");
    assert_eq!(
        reopen_and_check(dir.path(), "reopen after SQLITE_FULL"),
        Some(baseline()),
        "reopen must recover exactly the pre-fault state"
    );
}

/// A filesystem that writes part of a WAL frame and reports success. SQLite
/// cannot detect this at commit time — the WAL frame checksum catches it, and
/// only on recovery. The invariant under test is therefore not "the commit
/// fails" but "no torn state survives: all or nothing".
#[test]
fn short_write_on_wal_never_leaves_a_torn_commit() {
    let dir = tempfile::tempdir().unwrap();
    let space = seed(dir.path());
    let store = MemoryStore::open(&config(), dir.path()).unwrap();

    let fault = fault_vfs::arm(
        "short write on WAL",
        FaultSpec::once(
            dir.path(),
            MEMORY_DB_FILE,
            FileKind::Wal,
            OpKind::Write,
            Effect::ShortWrite { keep: 8 },
        ),
    );

    let result = remember(&store, space, "short-1", "ShortWrite", "torn or absent");
    fault.assert_fired();
    fault.disarm();
    drop(store);

    assert_structural_invariants(dir.path(), "after short write");
    let rows = reopen_and_check(dir.path(), "reopen after short write")
        .expect("a short WAL write must not make the store unopenable");

    let recorded = changeset_state(dir.path(), "short-1");
    let visible = rows.contains(&"ShortWrite|torn or absent".to_owned());
    // All-or-nothing across the audit trail and the canonical projection: a
    // durable changeset row without its content (or the reverse) is exactly
    // the torn state this fault looks for.
    assert_eq!(
        recorded.is_some(),
        visible,
        "changeset row {recorded:?} disagrees with canonical visibility {visible}; \
         the commit was torn by the short write (submit returned {result:?})"
    );
    for row in baseline() {
        assert!(
            rows.contains(&row),
            "the pre-fault baseline must survive a short write on a later commit: {rows:?}"
        );
    }
}

/// `SQLITE_IOERR_FSYNC` on the main database. With `synchronous=NORMAL` in
/// WAL mode SQLite performs no WAL fsync at commit — the durability barrier
/// is the checkpoint. That is where this fault therefore lands, which is a
/// fact about the production pragma set, not a limitation of the harness.
#[test]
fn sync_failure_during_checkpoint_fails_closed_and_keeps_data() {
    let dir = tempfile::tempdir().unwrap();
    let space = seed(dir.path());
    let store = MemoryStore::open(&config(), dir.path()).unwrap();
    store.set_wal_autocheckpoint(0).unwrap();
    remember(
        &store,
        space,
        "sync-1",
        "SyncFault",
        "committed before sync fault",
    )
    .unwrap();

    let fault = fault_vfs::arm(
        "fsync failure on main database",
        FaultSpec::once(
            dir.path(),
            MEMORY_DB_FILE,
            FileKind::MainDb,
            OpKind::Sync,
            Effect::Fail(SQLITE_IOERR_FSYNC),
        ),
    );

    let report = store.wal_checkpoint();
    fault.assert_fired();
    fault.disarm();
    assert!(
        report.is_err() || !report.as_ref().unwrap().complete,
        "a checkpoint whose fsync failed must not report a complete, durable drain: {report:?}"
    );

    // The commit predates the fault and must remain readable: the WAL still
    // holds it whether or not the checkpoint drained.
    let mut expected = baseline();
    expected.push("SyncFault|committed before sync fault".to_owned());
    expected.sort();
    drop(store);

    assert_structural_invariants(dir.path(), "after fsync failure");
    assert_eq!(
        reopen_and_check(dir.path(), "reopen after fsync failure"),
        Some(expected),
        "a failed checkpoint fsync must not lose an already-committed change"
    );
}

/// Disk-full while the checkpointer copies WAL pages into the database.
#[test]
fn checkpoint_write_failure_retains_the_wal_and_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let space = seed(dir.path());
    let store = MemoryStore::open(&config(), dir.path()).unwrap();
    store.set_wal_autocheckpoint(0).unwrap();
    remember(
        &store,
        space,
        "ckpt-1",
        "Checkpoint",
        "must survive checkpoint failure",
    )
    .unwrap();

    let fault = fault_vfs::arm(
        "SQLITE_FULL while checkpointing into the main database",
        FaultSpec::once(
            dir.path(),
            MEMORY_DB_FILE,
            FileKind::MainDb,
            OpKind::Write,
            Effect::Fail(SQLITE_FULL),
        ),
    );

    let report = store.wal_checkpoint();
    fault.assert_fired();
    fault.disarm();
    assert!(
        report.is_err() || !report.as_ref().unwrap().complete,
        "a checkpoint that could not write every page must not report complete: {report:?}"
    );

    let mut expected = baseline();
    expected.push("Checkpoint|must survive checkpoint failure".to_owned());
    expected.sort();

    // Still readable through the open handle: a failed checkpoint is a
    // maintenance failure, not a data-loss event.
    assert_eq!(
        canonical(&store),
        expected,
        "committed data must remain readable after a failed checkpoint"
    );
    drop(store);

    assert_structural_invariants(dir.path(), "after checkpoint write failure");
    assert_eq!(
        reopen_and_check(dir.path(), "reopen after checkpoint write failure"),
        Some(expected),
        "reopen must replay the retained WAL"
    );
}

/// The WAL file cannot be created or opened at all.
#[test]
fn wal_open_failure_makes_store_open_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let fault = fault_vfs::arm(
        "WAL file cannot be opened",
        FaultSpec {
            fires: u64::MAX,
            ..FaultSpec::once(
                dir.path(),
                MEMORY_DB_FILE,
                FileKind::Wal,
                OpKind::Open,
                Effect::Fail(SQLITE_CANTOPEN),
            )
        },
    );

    let opened = MemoryStore::open(&config(), dir.path());
    let failed_cleanly = opened.is_err();
    drop(opened);
    fault.assert_fired();
    fault.disarm();

    assert!(
        failed_cleanly,
        "a store that cannot open its WAL must fail rather than run in an \
         unknown journal mode"
    );
    assert_structural_invariants(dir.path(), "after WAL open failure");
    assert_eq!(
        reopen_and_check(dir.path(), "reopen after WAL open failure"),
        Some(baseline()),
        "the store must open normally once the WAL can be opened again"
    );
}

/// Build a store whose b-tree spans many pages, checkpointed so the content
/// lives in the main database file rather than the WAL.
fn seed_multi_page(dir: &Path) -> (SpaceId, Vec<String>) {
    let space = seed(dir);
    {
        let store = MemoryStore::open(&config(), dir).unwrap();
        for index in 0..64 {
            remember(
                &store,
                space,
                &format!("corrupt-seed-{index}"),
                "Corrupt",
                &format!("padding observation number {index} with some body text"),
            )
            .unwrap();
        }
        assert!(store.wal_checkpoint().unwrap().complete);
    }
    // A second open-and-close so the on-disk state the calibrated tests below
    // measure is the steady state, not the state right after a bulk load.
    let before = {
        let store = MemoryStore::open(&config(), dir).unwrap();
        canonical(&store)
    };
    assert!(before.len() > 60, "the corpus must span multiple pages");
    (space, before)
}

/// One page read returns corrupted bytes while the store is opening.
/// `min_offset` skips page 1, so the flipped byte is the page-header byte of
/// a b-tree page, which SQLite validates.
///
/// This injects *transient* corruption: the bytes on disk are untouched, so
/// the reopen assertion proves the store fails closed on a bad read rather
/// than persisting the damage. It does not prove anything about durable
/// on-disk corruption — SQLite's page format, not this store, is what would
/// have to detect that.
#[test]
fn corrupt_page_read_during_open_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (_space, before) = seed_multi_page(dir.path());

    // The store must be opened *after* the fault is armed: a warm page cache
    // serves reads without ever calling into the VFS, which is precisely how
    // an earlier version of this test managed to inject nothing.
    let fault = fault_vfs::arm(
        "corrupt page read during open",
        FaultSpec {
            min_offset: 4_096,
            ..FaultSpec::once(
                dir.path(),
                MEMORY_DB_FILE,
                FileKind::MainDb,
                OpKind::Read,
                Effect::CorruptRead { mask: 0xFF },
            )
        },
    );

    let opened = MemoryStore::open(&config(), dir.path());
    let observed: Option<Result<Vec<String>, MemoryError>> = opened.as_ref().ok().map(|store| {
        store
            .list_entities(None, 10_000, 0)
            .map(|rows| rows.into_iter().map(|row| row.entity.name).collect())
    });
    fault.assert_fired();
    // Disarm before the store drops: closing checkpoints, and a corrupted
    // read written back would turn transient damage into durable damage,
    // which is not what this test is about.
    fault.disarm();
    drop(opened);

    if let Some(Ok(names)) = observed {
        // Succeeding is allowed only if the answer is still correct. Serving a
        // plausible but wrong result from a corrupted page is the failure this
        // test looks for.
        assert_eq!(
            distinct_names(&names),
            distinct_names_of(&before),
            "a corrupted page read produced a wrong-but-plausible result \
             instead of failing closed"
        );
    }

    assert_structural_invariants(dir.path(), "after corrupt page read during open");
    assert_eq!(
        reopen_and_check(dir.path(), "reopen after corrupt page read during open"),
        Some(before),
        "transient read corruption must not be persisted"
    );
}

/// The same corruption, but placed on the *read* path rather than the open
/// path. The `nth` is calibrated from the harness's own match counter — a
/// never-firing probe counts the qualifying page reads one open performs, so
/// the real fault can skip exactly those and land on the scan. Hard-coding
/// that number would rot the first time the schema or migration changed.
#[test]
fn corrupt_page_read_during_scan_fails_closed_and_reopen_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let (_space, before) = seed_multi_page(dir.path());

    let reads_during_open = {
        let probe = fault_vfs::arm(
            "calibration probe (never fires)",
            FaultSpec {
                nth: u64::MAX,
                min_offset: 4_096,
                ..FaultSpec::once(
                    dir.path(),
                    MEMORY_DB_FILE,
                    FileKind::MainDb,
                    OpKind::Read,
                    Effect::CorruptRead { mask: 0xFF },
                )
            },
        );
        let store = MemoryStore::open(&config(), dir.path()).unwrap();
        let counted = probe.matched();
        assert_eq!(probe.fired(), 0, "the calibration probe must not inject");
        drop(store);
        counted
    };

    let fault = fault_vfs::arm(
        "corrupt page read during scan",
        FaultSpec {
            nth: reads_during_open + 1,
            min_offset: 4_096,
            ..FaultSpec::once(
                dir.path(),
                MEMORY_DB_FILE,
                FileKind::MainDb,
                OpKind::Read,
                Effect::CorruptRead { mask: 0xFF },
            )
        },
    );

    let store = MemoryStore::open(&config(), dir.path())
        .expect("the calibrated fault must not land on the open path");
    let observed: Result<Vec<String>, MemoryError> = store
        .list_entities(None, 10_000, 0)
        .map(|rows| rows.into_iter().map(|row| row.entity.name).collect());
    fault.assert_fired();
    fault.disarm();
    drop(store);

    if let Ok(names) = observed {
        assert_eq!(
            distinct_names(&names),
            distinct_names_of(&before),
            "a corrupted page read produced a wrong-but-plausible result \
             instead of failing closed"
        );
    }

    assert_structural_invariants(dir.path(), "after corrupt page read during scan");
    assert_eq!(
        reopen_and_check(dir.path(), "reopen after corrupt page read during scan"),
        Some(before),
        "transient read corruption must not be persisted"
    );
}

fn distinct_names(names: &[String]) -> Vec<String> {
    let mut names = names.to_vec();
    names.sort();
    names.dedup();
    names
}

/// Entity names from the `entity|content` rows [`canonical`] produces.
fn distinct_names_of(rows: &[String]) -> Vec<String> {
    distinct_names(
        &rows
            .iter()
            .map(|row| row.split('|').next().unwrap().to_owned())
            .collect::<Vec<_>>(),
    )
}

/// Reopen while the fault is still armed, then again once it is cleared.
/// Covers the "reopen-after-fault" leg explicitly rather than only as the
/// tail of another test.
#[test]
fn reopen_after_fault_recovers_or_reports_a_clean_failure() {
    let dir = tempfile::tempdir().unwrap();
    let space = seed(dir.path());

    // Fault the commit, leave the store open and the fault armed, then open a
    // second handle onto the same files.
    let store = MemoryStore::open(&config(), dir.path()).unwrap();
    let fault = fault_vfs::arm(
        "SQLITE_FULL held across a reopen",
        FaultSpec {
            fires: u64::MAX,
            ..FaultSpec::once(
                dir.path(),
                MEMORY_DB_FILE,
                FileKind::Wal,
                OpKind::Write,
                Effect::Fail(SQLITE_FULL),
            )
        },
    );
    assert!(remember(&store, space, "reopen-1", "Reopen", "not durable").is_err());
    fault.assert_fired();

    // A second handle opened while writes still fail must not corrupt or
    // truncate anything; it may legitimately fail to open.
    let second = MemoryStore::open(&config(), dir.path());
    if let Ok(second) = &second {
        assert_eq!(
            canonical(second),
            baseline(),
            "a handle opened during a write fault must not see the failed change"
        );
    }
    drop(second);
    drop(store);
    fault.disarm();

    assert_eq!(
        changeset_state(dir.path(), "reopen-1"),
        None,
        "the failed changeset must not appear after the fault clears"
    );
    assert_structural_invariants(dir.path(), "reopen under fault");
    assert_eq!(
        reopen_and_check(dir.path(), "reopen after fault cleared"),
        Some(baseline()),
        "the store must recover its exact pre-fault state"
    );

    // And it must be writable again, with the audit trail intact.
    let store = MemoryStore::open(&config(), dir.path()).unwrap();
    remember(
        &store,
        space,
        "reopen-2",
        "Reopen",
        "durable after recovery",
    )
    .unwrap();
    let mut expected = baseline();
    expected.push("Reopen|durable after recovery".to_owned());
    expected.sort();
    assert_eq!(canonical(&store), expected);
    drop(store);
    assert_structural_invariants(dir.path(), "after recovery write");
}

/// The derived index must never be quietly stale. This fault lands on the
/// *index publication* transaction rather than the canonical commit: the
/// changeset is durably applied, but the outbox rows that mark it indexed are
/// not. `recall` must then fail closed rather than serve results missing the
/// change, and an explicit repair must fix it.
///
/// The other nine tests never reach this state — every one of them leaves
/// `index_repair_required` clear, so their `assert_recall_matches_index_state`
/// only ever exercises the healthy branch. This test is what makes the
/// fail-closed branch actually covered.
///
/// Targeting is self-calibrating. `submit_changeset` commits canonically and
/// then drains the outbox in a second write transaction, so the *last* WAL
/// write of a submit belongs to the publication step. A never-firing probe
/// counts the WAL writes one identically shaped submit performs, and the real
/// fault is armed at that count. Measured on this store a submit performs 54
/// WAL writes, the last 6 of which are the publication step, but nothing
/// here hard-codes either number: a schema change moves the boundary and the
/// calibration moves with it.
#[test]
fn index_publication_failure_leaves_recall_fail_closed_then_repairs() {
    let dir = tempfile::tempdir().unwrap();
    let space = seed(dir.path());
    let store = MemoryStore::open(&config(), dir.path()).unwrap();
    store.set_wal_autocheckpoint(0).unwrap();

    // One warm-up submit first: the first audited write after an open does
    // extra one-time work (lazy baseline rows), so calibrating on it would
    // over-count and the fault would never fire.
    remember(
        &store,
        space,
        "warm-up",
        "Publish",
        "warm up the write path",
    )
    .unwrap();

    let writes_per_submit = {
        let probe = fault_vfs::arm(
            "calibration probe (never fires)",
            FaultSpec {
                nth: u64::MAX,
                ..FaultSpec::once(
                    dir.path(),
                    MEMORY_DB_FILE,
                    FileKind::Wal,
                    OpKind::Write,
                    Effect::Fail(SQLITE_IOERR_WRITE),
                )
            },
        );
        remember(&store, space, "calibrate", "Publish", "calibration write").unwrap();
        let counted = probe.matched();
        assert_eq!(probe.fired(), 0, "the calibration probe must not inject");
        assert!(counted > 1, "a submit must perform more than one WAL write");
        counted
    };

    let fault = fault_vfs::arm(
        "index publication write failure",
        FaultSpec {
            nth: writes_per_submit,
            ..FaultSpec::once(
                dir.path(),
                MEMORY_DB_FILE,
                FileKind::Wal,
                OpKind::Write,
                Effect::Fail(SQLITE_IOERR_WRITE),
            )
        },
    );
    let submitted = remember(
        &store,
        space,
        "publish-1",
        "Publish",
        "canonically committed",
    );
    fault.assert_fired();
    fault.disarm();

    // The canonical commit is durable; only the derived-state publication
    // failed. If a future change moves the publication ahead of the last
    // write this assertion fails loudly rather than degrading into a test of
    // the already-covered canonical-abort path.
    assert!(
        submitted.is_ok(),
        "the fault landed on the canonical commit, not the index publication; \
         recalibrate before trusting this test: {submitted:?}"
    );
    let pending: i64 = read_only(dir.path())
        .query_row(
            "SELECT COUNT(*) FROM index_outbox WHERE applied_at IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        pending > 0,
        "the index publication must have been left undrained"
    );

    // Fail closed: a stale derived index must not serve partial results.
    let mut filters = RecallFilters::new();
    filters.mode = Some(SearchMode::KeywordOnly);
    assert!(
        matches!(
            store.recall("canonically", 5, &filters),
            Err(MemoryError::IndexRepairRequired)
        ),
        "recall must refuse to serve while the derived index is stale"
    );

    // And repair must be able to clear it.
    store.repair_index().unwrap();
    assert!(
        store.recall("canonically", 5, &filters).is_ok(),
        "recall must work again once repair has drained the outbox"
    );
    let mut expected = baseline();
    expected.push("Publish|warm up the write path".to_owned());
    expected.push("Publish|calibration write".to_owned());
    expected.push("Publish|canonically committed".to_owned());
    expected.sort();
    assert_eq!(
        canonical(&store),
        expected,
        "the canonical commit must have survived the publication failure"
    );
    drop(store);

    assert_structural_invariants(dir.path(), "after index publication failure");
    assert_eq!(
        reopen_and_check(dir.path(), "reopen after index publication failure"),
        Some(expected),
        "reopen must see the repaired store"
    );
}
