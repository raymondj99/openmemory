//! Read-only `rusqlite::Connection` pool for the memory store.
//!
//! v0.1's [`MemoryStore`](crate::store::MemoryStore) wrapped a single
//! `Mutex<Connection>`, which serialised every read against every write —
//! fine for a single agent, painful when multiple agents (or a web UI plus
//! an MCP client) share the same on-disk database. SQLite is already the
//! cross-process arbiter; the v0.2 fix is to give the *in-process* store a
//! pool of dedicated read-only handles so concurrent recalls run in
//! parallel.
//!
//! WAL mode is the enabler. The writer commits to the WAL without taking
//! the database file lock; readers consult the WAL through their own end
//! mark and never block the writer (see <https://www.sqlite.org/wal.html>).
//! Each read-only connection here is opened once at store-open time with
//! `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX`. `OPEN_NO_MUTEX` is safe
//! because rusqlite enforces threading via `Connection: Send + !Sync` —
//! every pool slot owns its handle exclusively.
//!
//! ## API
//!
//! [`ReadPool::with_reader`] hands the caller a `&Connection` for the
//! duration of the closure, blocking only when every connection is busy.
//! The closure result is forwarded as-is. Pool order is FIFO: the
//! least-recently-used connection is returned next, which keeps SQLite's
//! per-connection page cache hot uniformly across slots.
//!
//! ## In-memory fallback
//!
//! `Connection::open_in_memory` creates a private database that a separate
//! handle cannot reach. For tests that build a store with
//! [`MemoryStore::open_in_memory`](crate::store::MemoryStore::open_in_memory)
//! we therefore degrade the pool to a single slot that proxies to the
//! writer connection's mutex. Read concurrency is lost on that path, but
//! semantic equivalence is preserved.
//!
//! ## Lock lifetime
//!
//! - `available` (`Mutex<Vec<Connection>>`): held only across the pop /
//!   push around `with_reader`'s closure. Never held across SQLite calls.
//! - `cond_var` (`Condvar`): wakes a single waiter when a connection is
//!   returned. No fairness beyond stdlib's semantics; SQLite work
//!   dominates the wait time.

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};

use rusqlite::{Connection, OpenFlags};

use crate::error::{MemoryError, MemoryResult};

/// A small pool of read-only [`Connection`]s plus a condvar to block
/// callers when every slot is busy. See the module-level docs for the
/// design rationale.
pub struct ReadPool {
    inner: ReadPoolInner,
}

enum ReadPoolInner {
    /// Pool of dedicated read-only connections opened against an on-disk
    /// SQLite database. Every slot is fully independent: SQLite serialises
    /// internally via WAL frames, not via the rusqlite handle.
    Multi {
        available: Mutex<Vec<Connection>>,
        cond_var: Condvar,
        size: usize,
    },
    /// Degenerate pool that proxies to the writer's mutex. Used when the
    /// underlying database is `:memory:`, where a fresh `Connection` would
    /// open an *unrelated* in-memory database.
    SharedWriter(Arc<Mutex<Connection>>),
}

impl ReadPool {
    /// Open `size` read-only handles to `path`. `size` is clamped to at
    /// least one slot. Each handle gets the read-friendly pragmas
    /// (`busy_timeout`, `cache_size`); journal mode is set on the writer
    /// and inherited from the database file, so we don't try to set it
    /// here (read-only handles can't change WAL mode anyway).
    pub fn open(path: &Path, size: usize) -> MemoryResult<Self> {
        let size = size.max(1);
        let mut connections = Vec::with_capacity(size);
        for _ in 0..size {
            let conn = open_reader(path)?;
            connections.push(conn);
        }
        Ok(Self {
            inner: ReadPoolInner::Multi {
                available: Mutex::new(connections),
                cond_var: Condvar::new(),
                size,
            },
        })
    }

    /// Build a pool that delegates every read to the writer's mutex. Used
    /// by the in-memory test path. Keeps the call-site contract identical
    /// to [`Self::open`] without forcing tests to special-case a missing
    /// pool.
    #[must_use]
    pub fn shared_with_writer(writer: Arc<Mutex<Connection>>) -> Self {
        Self {
            inner: ReadPoolInner::SharedWriter(writer),
        }
    }

    /// Number of connections in the pool. Returns `1` for the shared-writer
    /// variant — that's the effective concurrency available to readers.
    #[must_use]
    pub fn size(&self) -> usize {
        match &self.inner {
            ReadPoolInner::Multi { size, .. } => *size,
            ReadPoolInner::SharedWriter(_) => 1,
        }
    }

    /// Run `f` against a borrowed read-only connection. Blocks only when
    /// every slot is busy, then resumes the moment a slot is returned.
    /// Errors from `f` are propagated; the connection is unconditionally
    /// returned to the pool, even when `f` panics (the guard handles
    /// release on drop).
    pub fn with_reader<F, R>(&self, f: F) -> MemoryResult<R>
    where
        F: FnOnce(&Connection) -> MemoryResult<R>,
    {
        match &self.inner {
            ReadPoolInner::Multi {
                available,
                cond_var,
                ..
            } => {
                let conn = checkout(available, cond_var);
                let guard = ReturnGuard {
                    available,
                    cond_var,
                    conn: Some(conn),
                };
                let conn_ref = guard.conn.as_ref().expect("guard holds a connection");
                f(conn_ref)
            }
            ReadPoolInner::SharedWriter(writer) => {
                let lock = writer.lock().unwrap_or_else(|e| e.into_inner());
                f(&lock)
            }
        }
    }
}

impl std::fmt::Debug for ReadPool {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            ReadPoolInner::Multi { size, .. } => fmt
                .debug_struct("ReadPool::Multi")
                .field("size", size)
                .finish(),
            ReadPoolInner::SharedWriter(_) => fmt.debug_struct("ReadPool::SharedWriter").finish(),
        }
    }
}

/// RAII guard that returns a connection to the pool on drop. Splitting
/// this from `with_reader` lets the closure run with `&Connection` while
/// still releasing the slot if the closure panics.
struct ReturnGuard<'a> {
    available: &'a Mutex<Vec<Connection>>,
    cond_var: &'a Condvar,
    conn: Option<Connection>,
}

impl Drop for ReturnGuard<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let mut slots = self.available.lock().unwrap_or_else(|e| e.into_inner());
            slots.push(conn);
            // notify_one is enough: one waiter can grab the freed slot.
            self.cond_var.notify_one();
        }
    }
}

fn checkout(available: &Mutex<Vec<Connection>>, cond_var: &Condvar) -> Connection {
    let mut slots = available.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(conn) = slots.pop() {
            return conn;
        }
        slots = cond_var.wait(slots).unwrap_or_else(|e| e.into_inner());
    }
}

fn open_reader(path: &Path) -> MemoryResult<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)
        .map_err(|e| MemoryError::Schema(format!("open read-only connection: {e}")))?;
    // Read-only handles can't set journal_mode, but session-local pragmas
    // are fair game. busy_timeout matches the writer's setting so a reader
    // mid-snapshot waits cooperatively rather than failing fast.
    conn.execute_batch(
        "PRAGMA busy_timeout=5000;
         PRAGMA cache_size=-8000;
         PRAGMA query_only=1;",
    )?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use std::sync::Arc;
    use std::thread;

    fn seed(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
             INSERT INTO t (id, name) VALUES (1, 'alpha'), (2, 'beta');",
        )
        .unwrap();
    }

    #[test]
    fn open_creates_a_pool_of_independent_handles() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("p.sqlite");
        seed(&db);

        let pool = ReadPool::open(&db, 4).unwrap();
        assert_eq!(pool.size(), 4);

        let count: i64 = pool
            .with_reader(|conn| {
                let n: i64 = conn
                    .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
                    .unwrap();
                Ok(n)
            })
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn size_zero_is_clamped_to_one() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("p.sqlite");
        seed(&db);
        let pool = ReadPool::open(&db, 0).unwrap();
        assert_eq!(pool.size(), 1);
    }

    #[test]
    fn shared_writer_proxies_through_mutex() {
        let writer = Connection::open_in_memory().unwrap();
        writer
            .execute_batch(
                "CREATE TABLE t (id INTEGER PRIMARY KEY);
                 INSERT INTO t (id) VALUES (1), (2), (3);",
            )
            .unwrap();
        let writer = Arc::new(Mutex::new(writer));
        let pool = ReadPool::shared_with_writer(writer);
        assert_eq!(pool.size(), 1);

        let n: i64 = pool
            .with_reader(|conn| {
                let n: i64 = conn
                    .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
                    .unwrap();
                Ok(n)
            })
            .unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn concurrent_readers_complete_without_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("p.sqlite");
        seed(&db);

        let pool = Arc::new(ReadPool::open(&db, 4).unwrap());
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    pool.with_reader(|conn| {
                        for _ in 0..500 {
                            let _: i64 = conn
                                .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
                                .unwrap();
                        }
                        Ok(())
                    })
                    .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        // The strict wall-clock assertion lives in the recall-level
        // integration test (commit 5) where the SQL is representative of
        // a real workload; this smoke test only proves the slots stay
        // independent enough to finish.
    }

    #[test]
    fn waiters_unblock_when_a_slot_is_returned() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("p.sqlite");
        seed(&db);

        // Pool of one — both threads contend for the same slot. The
        // barrier forces them to start `with_reader` at (very nearly) the
        // same time, so whichever thread loses the race must park on the
        // condvar and wake when the other thread's `ReturnGuard::drop`
        // fires. If the wakeup path is broken, this test deadlocks rather
        // than ticking through silently.
        let pool = Arc::new(ReadPool::open(&db, 1).unwrap());
        let gate = Arc::new(std::sync::Barrier::new(2));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let pool = Arc::clone(&pool);
            let gate = Arc::clone(&gate);
            handles.push(thread::spawn(move || -> i64 {
                gate.wait();
                pool.with_reader(|conn| {
                    let n: i64 = conn
                        .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
                        .unwrap();
                    Ok(n)
                })
                .unwrap()
            }));
        }
        for h in handles {
            assert_eq!(h.join().unwrap(), 2);
        }
    }

    #[test]
    fn reader_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("p.sqlite");
        seed(&db);

        let pool = ReadPool::open(&db, 1).unwrap();
        let result: MemoryResult<usize> = pool.with_reader(|conn| {
            // OPEN_READ_ONLY surfaces as `attempt to write a readonly database`.
            let n = conn.execute("INSERT INTO t (id, name) VALUES (3, 'gamma')", params![])?;
            Ok(n)
        });
        assert!(
            result.is_err(),
            "read-only handle must reject writes: {result:?}"
        );
    }
}
