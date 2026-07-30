//! Test-only controllable SQLite VFS.
//!
//! This module registers a pass-through VFS shim as the process default
//! before any store is opened, so faults are injected *below* SQLite and the
//! production code path above it — `MemoryStore::open`, `submit_changeset`,
//! `wal_checkpoint`, `repair_index`, `recall` — runs unmodified. Nothing in
//! the library changes: there is no injection seam, no test hook, and no
//! `Connection::open_with_flags_and_vfs` call in production code.
//!
//! ## Why unsafe is unavoidable here
//!
//! A SQLite VFS is a C vtable of `extern "C"` function pointers plus a
//! caller-allocated file object whose size the VFS declares. rusqlite 0.32
//! offers no safe binding for `sqlite3_vfs_register`; it re-exports the raw
//! bindings as `rusqlite::ffi` and nothing more. The shim is therefore
//! written directly against `rusqlite::ffi`. It is compiled only into the
//! `vfs_fault_matrix` integration-test binary — the library itself keeps
//! `#![forbid(unsafe_code)]` and gains no new API surface.
//!
//! ## Determinism
//!
//! Every fault is `(directory, base file name, file kind, operation,
//! minimum offset, nth match, fire budget, effect)`. Nothing is random and
//! nothing is time-based: the *n*-th matching operation on one named file
//! fails, at most `fires` times. Rules are keyed by directory, so tests
//! running in parallel in the same binary cannot see each other's faults.
//!
//! ## Proving the harness injects
//!
//! Every rule carries two counters. `matched` counts operations the rule's
//! predicate accepted; `fired` counts times the effect was actually applied.
//! A fault test that stops injecting reports `fired == 0` and fails. The
//! shim separately counts *all* intercepted operations per watched directory
//! ([`observed`]), which is what proves the shim is on the production path at
//! all rather than silently bypassed.
//!
//! ## Deliberate deviation from the host VFS
//!
//! [`shim_fetch`] always reports "no memory map available". SQLite then
//! reads through `xRead`, which is what makes read-side fault injection
//! complete. This matches production behaviour: `PRAGMA mmap_size` defaults
//! to 0 in this build and the store never raises it, so no covered path
//! loses coverage.

// See the module docs: the C VFS contract cannot be expressed safely with
// the dependencies available. Test-binary only.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};

use rusqlite::ffi;

/// Name the shim registers under. Also the assertion target for
/// "is the shim actually the default VFS".
pub const SHIM_VFS_NAME: &str = "openmemory-fault-shim";

/// Which SQLite file a rule targets. Derived from the `xOpen` flags, not
/// from the file name, so a renamed journal cannot silently miss.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// The database file itself (`memory.sqlite`).
    MainDb,
    /// The write-ahead log (`memory.sqlite-wal`).
    Wal,
    /// A rollback or super journal.
    Journal,
    /// Temporary and transient files; never a fault target.
    Other,
}

/// Which VFS entry point a rule targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OpKind {
    Open,
    Read,
    Write,
    Truncate,
    Sync,
}

/// What the shim does when a rule fires.
#[derive(Clone, Copy, Debug)]
pub enum Effect {
    /// Return this SQLite result code without performing the operation.
    Fail(c_int),
    /// Write only the first `keep` bytes of the buffer and report success —
    /// a filesystem that lies about a partial write. `keep` is clamped below
    /// the requested amount so the fault is always genuinely short.
    ShortWrite { keep: usize },
    /// Perform the read, then flip bits in the first byte of the returned
    /// buffer. Combined with `min_offset` this targets the page-type byte of
    /// a specific b-tree page, which SQLite validates.
    CorruptRead { mask: u8 },
}

/// A fault to arm. Every field is explicit; there are no defaults that could
/// silently widen a rule's blast radius.
#[derive(Clone, Debug)]
pub struct FaultSpec {
    /// Directory holding the target database. Rules never match outside it.
    pub dir: PathBuf,
    /// Base database file name, e.g. `memory.sqlite`. A `-wal`/`-shm`/
    /// `-journal` suffix is stripped before comparison, so one base name
    /// covers the whole file family.
    pub base: String,
    pub kind: FileKind,
    pub op: OpKind,
    /// Fire on the `nth` matching operation, 1-based.
    pub nth: u64,
    /// How many times the effect may be applied. Usually 1.
    pub fires: u64,
    /// Only match operations at or after this file offset. Used to skip the
    /// database header when corrupting a page. Ignored for `Open`/`Sync`.
    pub min_offset: i64,
    pub effect: Effect,
}

impl FaultSpec {
    /// A one-shot fault on the first matching operation.
    pub fn once(dir: &Path, base: &str, kind: FileKind, op: OpKind, effect: Effect) -> Self {
        Self {
            dir: dir.to_path_buf(),
            base: base.to_owned(),
            kind,
            op,
            nth: 1,
            fires: 1,
            min_offset: 0,
            effect,
        }
    }
}

#[derive(Debug)]
struct Rule {
    spec: FaultSpec,
    matched: AtomicU64,
    fired: AtomicU64,
}

/// RAII owner of an armed fault. Dropping it disarms the rule, so a test
/// that panics cannot leak a fault into a parallel test.
#[derive(Debug)]
pub struct FaultHandle {
    rule: Arc<Rule>,
    label: String,
}

impl FaultHandle {
    /// Operations whose predicate the rule accepted, whether or not the
    /// effect was applied.
    pub fn matched(&self) -> u64 {
        self.rule.matched.load(Ordering::SeqCst)
    }

    /// Times the effect was actually applied.
    pub fn fired(&self) -> u64 {
        self.rule.fired.load(Ordering::SeqCst)
    }

    /// The check that keeps this suite honest: a fault-injection test that
    /// silently stops injecting passes everything and proves nothing.
    pub fn assert_fired(&self) {
        assert!(
            self.fired() > 0,
            "fault {} never fired ({} operations matched its predicate); \
             the harness stopped injecting and this test proves nothing",
            self.label,
            self.matched()
        );
    }

    /// Disarm early, while keeping the counters readable.
    pub fn disarm(&self) {
        registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rules
            .retain(|rule| !Arc::ptr_eq(rule, &self.rule));
    }
}

impl Drop for FaultHandle {
    fn drop(&mut self) {
        self.disarm();
    }
}

#[derive(Default)]
struct Registry {
    rules: Vec<Arc<Rule>>,
    watched: Vec<PathBuf>,
    observed: HashMap<(PathBuf, String, FileKind, OpKind), u64>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

/// Resolve a directory the way SQLite's `xFullPathname` does before it hands
/// a name to `xOpen`. On macOS `TMPDIR` lives under `/var`, which is a
/// symlink to `/private/var`; without this both sides of every rule
/// comparison would be spelled differently and no fault could ever match.
/// The harness self-tests are what caught that.
fn normalized(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// Start counting every intercepted operation on files under `dir`. Without
/// this, [`observed`] returns 0 and cannot distinguish "not watched" from
/// "not on the path" — so tests must watch before they assert.
pub fn watch(dir: &Path) {
    let mut guard = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = normalized(dir);
    if !guard.watched.contains(&dir) {
        guard.watched.push(dir);
    }
}

/// Intercepted operation count for one file family under a watched
/// directory. This is the evidence that the shim sits on the production
/// path, independent of whether any fault was armed.
pub fn observed(dir: &Path, base: &str, kind: FileKind, op: OpKind) -> u64 {
    let guard = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .observed
        .get(&(normalized(dir), base.to_owned(), kind, op))
        .copied()
        .unwrap_or(0)
}

/// Arm a fault. `label` appears in the failure message when the fault never
/// fires, so name it after the invariant under test.
pub fn arm(label: &str, spec: FaultSpec) -> FaultHandle {
    assert!(spec.nth >= 1, "nth is 1-based");
    assert!(spec.fires >= 1, "a rule that can never fire is a no-op");
    install();
    let spec = FaultSpec {
        dir: normalized(&spec.dir),
        ..spec
    };
    let rule = Arc::new(Rule {
        spec,
        matched: AtomicU64::new(0),
        fired: AtomicU64::new(0),
    });
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .rules
        .push(Arc::clone(&rule));
    FaultHandle {
        rule,
        label: label.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Shim state
// ---------------------------------------------------------------------------

/// Raw VFS pointer wrapper. SQLite owns the pointee for the process
/// lifetime; sharing it across threads is exactly what SQLite expects.
struct VfsPtr(*mut ffi::sqlite3_vfs);
unsafe impl Send for VfsPtr {}
unsafe impl Sync for VfsPtr {}

static REAL_VFS: OnceLock<VfsPtr> = OnceLock::new();
static SHIM_VFS: OnceLock<VfsPtr> = OnceLock::new();

fn real_vfs() -> *mut ffi::sqlite3_vfs {
    REAL_VFS.get().expect("shim not installed").0
}

/// Per-open-file identity, resolved once in `xOpen`.
struct FileState {
    dir: PathBuf,
    base: String,
    kind: FileKind,
}

/// The file object SQLite allocates for us. `base` must be first: SQLite
/// passes `*mut sqlite3_file` and we cast. The real VFS's file object lives
/// in the trailing bytes we asked for via `szOsFile`.
#[repr(C)]
struct ShimFile {
    base: ffi::sqlite3_file,
    real: *mut ffi::sqlite3_file,
    state: *mut FileState,
    methods: *mut ffi::sqlite3_io_methods,
}

/// Install the shim as the default VFS. Idempotent and safe to call from
/// several tests concurrently.
pub fn install() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| unsafe {
        // Force library init so the built-in VFS list exists before we look
        // the default up.
        assert_eq!(ffi::sqlite3_initialize(), ffi::SQLITE_OK);
        let real = ffi::sqlite3_vfs_find(std::ptr::null());
        assert!(!real.is_null(), "no default SQLite VFS to delegate to");
        REAL_VFS
            .set(VfsPtr(real))
            .unwrap_or_else(|_| panic!("real VFS already recorded"));

        // Leaked deliberately: SQLite keeps both the name and the vtable for
        // the remainder of the process.
        let name = CString::new(SHIM_VFS_NAME).unwrap().into_raw();
        let vfs = Box::into_raw(Box::new(ffi::sqlite3_vfs {
            iVersion: (*real).iVersion,
            szOsFile: c_int::try_from(std::mem::size_of::<ShimFile>()).unwrap() + (*real).szOsFile,
            mxPathname: (*real).mxPathname,
            pNext: std::ptr::null_mut(),
            zName: name,
            pAppData: std::ptr::null_mut(),
            xOpen: Some(shim_open),
            xDelete: (*real).xDelete.map(|_| shim_delete as _),
            xAccess: (*real).xAccess.map(|_| shim_access as _),
            xFullPathname: (*real).xFullPathname.map(|_| shim_full_pathname as _),
            xDlOpen: (*real).xDlOpen.map(|_| shim_dlopen as _),
            xDlError: (*real).xDlError.map(|_| shim_dlerror as _),
            xDlSym: (*real).xDlSym.map(|_| shim_dlsym as _),
            xDlClose: (*real).xDlClose.map(|_| shim_dlclose as _),
            xRandomness: (*real).xRandomness.map(|_| shim_randomness as _),
            xSleep: (*real).xSleep.map(|_| shim_sleep as _),
            xCurrentTime: (*real).xCurrentTime.map(|_| shim_current_time as _),
            xGetLastError: (*real).xGetLastError.map(|_| shim_get_last_error as _),
            xCurrentTimeInt64: (*real)
                .xCurrentTimeInt64
                .map(|_| shim_current_time_int64 as _),
            xSetSystemCall: (*real).xSetSystemCall.map(|_| shim_set_system_call as _),
            xGetSystemCall: (*real).xGetSystemCall.map(|_| shim_get_system_call as _),
            xNextSystemCall: (*real).xNextSystemCall.map(|_| shim_next_system_call as _),
        }));
        assert_eq!(
            ffi::sqlite3_vfs_register(vfs, 1),
            ffi::SQLITE_OK,
            "registering the fault shim as default VFS failed"
        );
        SHIM_VFS
            .set(VfsPtr(vfs))
            .unwrap_or_else(|_| panic!("shim VFS already recorded"));
    });
}

/// True when `sqlite3_vfs_find(NULL)` resolves to this shim — i.e. every
/// `Connection::open` in this process goes through it.
pub fn is_default_vfs() -> bool {
    install();
    unsafe {
        let current = ffi::sqlite3_vfs_find(std::ptr::null());
        if current.is_null() || current != SHIM_VFS.get().map_or(std::ptr::null_mut(), |p| p.0) {
            return false;
        }
        CStr::from_ptr((*current).zName).to_str() == Ok(SHIM_VFS_NAME)
    }
}

// ---------------------------------------------------------------------------
// Rule matching
// ---------------------------------------------------------------------------

/// Strip the `-wal` / `-shm` / `-journal` suffix so one base name covers a
/// whole SQLite file family.
fn base_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for suffix in ["-wal", "-shm", "-journal"] {
        if let Some(stem) = name.strip_suffix(suffix) {
            return stem.to_owned();
        }
    }
    name
}

fn kind_from_flags(flags: c_int) -> FileKind {
    if flags & ffi::SQLITE_OPEN_MAIN_DB != 0 {
        FileKind::MainDb
    } else if flags & ffi::SQLITE_OPEN_WAL != 0 {
        FileKind::Wal
    } else if flags
        & (ffi::SQLITE_OPEN_MAIN_JOURNAL
            | ffi::SQLITE_OPEN_TEMP_JOURNAL
            | ffi::SQLITE_OPEN_SUPER_JOURNAL
            | ffi::SQLITE_OPEN_SUBJOURNAL)
        != 0
    {
        FileKind::Journal
    } else {
        FileKind::Other
    }
}

/// Record the operation and decide whether a rule fires. Reserving the fire
/// slot here (rather than after the effect is applied) keeps `fired` exact
/// under the writer mutex the store already holds; every effect this module
/// defines is unconditionally applicable once selected.
fn select(dir: &Path, base: &str, kind: FileKind, op: OpKind, offset: i64) -> Option<Effect> {
    // Set `FAULT_VFS_DEBUG=1` to print every intercepted operation. This is
    // the fastest way to re-target a rule when the store's I/O sequence
    // changes; without it, "the fault never fired" gives no clue why.
    static DEBUG: OnceLock<bool> = OnceLock::new();
    if *DEBUG.get_or_init(|| std::env::var_os("FAULT_VFS_DEBUG").is_some()) {
        eprintln!("[fault-vfs] dir={dir:?} base={base:?} kind={kind:?} op={op:?} offset={offset}");
    }
    let mut guard = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.watched.iter().any(|watched| watched == dir) {
        *guard
            .observed
            .entry((dir.to_path_buf(), base.to_owned(), kind, op))
            .or_insert(0) += 1;
    }
    for rule in &guard.rules {
        let spec = &rule.spec;
        if spec.dir != dir || spec.base != base || spec.kind != kind || spec.op != op {
            continue;
        }
        if matches!(op, OpKind::Read | OpKind::Write) && offset < spec.min_offset {
            continue;
        }
        let seen = rule.matched.fetch_add(1, Ordering::SeqCst) + 1;
        if seen < spec.nth {
            continue;
        }
        if rule.fired.load(Ordering::SeqCst) >= spec.fires {
            continue;
        }
        rule.fired.fetch_add(1, Ordering::SeqCst);
        return Some(spec.effect);
    }
    None
}

// ---------------------------------------------------------------------------
// VFS methods
// ---------------------------------------------------------------------------

unsafe fn shim_parts<'a>(file: *mut ffi::sqlite3_file) -> (&'a FileState, *mut ffi::sqlite3_file) {
    let shim = file.cast::<ShimFile>();
    (&*(*shim).state, (*shim).real)
}

unsafe extern "C" fn shim_open(
    _vfs: *mut ffi::sqlite3_vfs,
    name: ffi::sqlite3_filename,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    let shim = file.cast::<ShimFile>();
    // Until the delegate succeeds, SQLite must see an unusable file object.
    (*shim).base.pMethods = std::ptr::null();
    // The delegate's file object lives in the trailing bytes we reserved via
    // `szOsFile`. Stepping one `ShimFile` forward (rather than casting
    // through `*mut u8`) keeps the pointer alignment provably sufficient.
    (*shim).real = shim.add(1).cast::<ffi::sqlite3_file>();
    (*shim).state = std::ptr::null_mut();
    (*shim).methods = std::ptr::null_mut();

    let kind = kind_from_flags(flags);
    let path = if name.is_null() {
        None
    } else {
        Some(PathBuf::from(
            CStr::from_ptr(name).to_string_lossy().into_owned(),
        ))
    };
    let identity = path.as_ref().and_then(|path| {
        path.parent()
            .map(|dir| (dir.to_path_buf(), base_name(path), kind))
    });

    if let Some((dir, base, kind)) = identity.as_ref() {
        if let Some(Effect::Fail(code)) = select(dir, base, *kind, OpKind::Open, 0) {
            return code;
        }
    }

    let real = real_vfs();
    let rc = ((*real).xOpen.unwrap())(real, name, (*shim).real, flags, out_flags);
    if rc != ffi::SQLITE_OK {
        return rc;
    }

    let (dir, base) = identity.map_or_else(
        || (PathBuf::new(), String::new()),
        |(dir, base, _)| (dir, base),
    );
    (*shim).state = Box::into_raw(Box::new(FileState { dir, base, kind }));
    (*shim).methods = Box::into_raw(Box::new(mirror_methods((*(*shim).real).pMethods)));
    (*shim).base.pMethods = (*shim).methods;
    ffi::SQLITE_OK
}

/// Build our vtable from the delegate's, slot by slot. A slot the host VFS
/// leaves empty stays empty here, so we never advertise a method the
/// delegate cannot service.
unsafe fn mirror_methods(real: *const ffi::sqlite3_io_methods) -> ffi::sqlite3_io_methods {
    ffi::sqlite3_io_methods {
        iVersion: (*real).iVersion,
        xClose: Some(shim_close),
        xRead: (*real).xRead.map(|_| shim_read as _),
        xWrite: (*real).xWrite.map(|_| shim_write as _),
        xTruncate: (*real).xTruncate.map(|_| shim_truncate as _),
        xSync: (*real).xSync.map(|_| shim_sync as _),
        xFileSize: (*real).xFileSize.map(|_| shim_file_size as _),
        xLock: (*real).xLock.map(|_| shim_lock as _),
        xUnlock: (*real).xUnlock.map(|_| shim_unlock as _),
        xCheckReservedLock: (*real)
            .xCheckReservedLock
            .map(|_| shim_check_reserved_lock as _),
        xFileControl: (*real).xFileControl.map(|_| shim_file_control as _),
        xSectorSize: (*real).xSectorSize.map(|_| shim_sector_size as _),
        xDeviceCharacteristics: (*real)
            .xDeviceCharacteristics
            .map(|_| shim_device_characteristics as _),
        xShmMap: (*real).xShmMap.map(|_| shim_shm_map as _),
        xShmLock: (*real).xShmLock.map(|_| shim_shm_lock as _),
        xShmBarrier: (*real).xShmBarrier.map(|_| shim_shm_barrier as _),
        xShmUnmap: (*real).xShmUnmap.map(|_| shim_shm_unmap as _),
        // Always answer "no mapping": see the module docs.
        xFetch: (*real).xFetch.map(|_| shim_fetch as _),
        xUnfetch: (*real).xUnfetch.map(|_| shim_unfetch as _),
    }
}

unsafe extern "C" fn shim_close(file: *mut ffi::sqlite3_file) -> c_int {
    let shim = file.cast::<ShimFile>();
    let real = (*shim).real;
    let rc = ((*(*real).pMethods).xClose.unwrap())(real);
    // SQLite clears pMethods after xClose returns and never dereferences it
    // again, so releasing our per-file allocations here is safe.
    if !(*shim).state.is_null() {
        drop(Box::from_raw((*shim).state));
        (*shim).state = std::ptr::null_mut();
    }
    (*shim).base.pMethods = std::ptr::null();
    if !(*shim).methods.is_null() {
        drop(Box::from_raw((*shim).methods));
        (*shim).methods = std::ptr::null_mut();
    }
    rc
}

unsafe extern "C" fn shim_read(
    file: *mut ffi::sqlite3_file,
    buf: *mut c_void,
    amt: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    let (state, real) = shim_parts(file);
    let effect = select(&state.dir, &state.base, state.kind, OpKind::Read, offset);
    if let Some(Effect::Fail(code)) = effect {
        return code;
    }
    let rc = ((*(*real).pMethods).xRead.unwrap())(real, buf, amt, offset);
    if rc == ffi::SQLITE_OK && amt > 0 {
        if let Some(Effect::CorruptRead { mask }) = effect {
            let first = buf.cast::<u8>();
            *first ^= mask;
        }
    }
    rc
}

unsafe extern "C" fn shim_write(
    file: *mut ffi::sqlite3_file,
    buf: *const c_void,
    amt: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    let (state, real) = shim_parts(file);
    let effect = select(&state.dir, &state.base, state.kind, OpKind::Write, offset);
    match effect {
        Some(Effect::Fail(code)) => code,
        Some(Effect::ShortWrite { keep }) => {
            // A filesystem that writes part of the buffer and reports
            // success. Clamped so the fault is always genuinely short.
            let keep = c_int::try_from(keep)
                .unwrap_or(c_int::MAX)
                .min(amt - 1)
                .max(0);
            let rc = ((*(*real).pMethods).xWrite.unwrap())(real, buf, keep, offset);
            if rc == ffi::SQLITE_OK {
                ffi::SQLITE_OK
            } else {
                rc
            }
        }
        _ => ((*(*real).pMethods).xWrite.unwrap())(real, buf, amt, offset),
    }
}

unsafe extern "C" fn shim_truncate(
    file: *mut ffi::sqlite3_file,
    size: ffi::sqlite3_int64,
) -> c_int {
    let (state, real) = shim_parts(file);
    if let Some(Effect::Fail(code)) = select(
        &state.dir,
        &state.base,
        state.kind,
        OpKind::Truncate,
        i64::MAX,
    ) {
        return code;
    }
    ((*(*real).pMethods).xTruncate.unwrap())(real, size)
}

unsafe extern "C" fn shim_sync(file: *mut ffi::sqlite3_file, flags: c_int) -> c_int {
    let (state, real) = shim_parts(file);
    if let Some(Effect::Fail(code)) =
        select(&state.dir, &state.base, state.kind, OpKind::Sync, i64::MAX)
    {
        return code;
    }
    ((*(*real).pMethods).xSync.unwrap())(real, flags)
}

unsafe extern "C" fn shim_file_size(
    file: *mut ffi::sqlite3_file,
    size: *mut ffi::sqlite3_int64,
) -> c_int {
    let (_, real) = shim_parts(file);
    ((*(*real).pMethods).xFileSize.unwrap())(real, size)
}

unsafe extern "C" fn shim_lock(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    let (_, real) = shim_parts(file);
    ((*(*real).pMethods).xLock.unwrap())(real, level)
}

unsafe extern "C" fn shim_unlock(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    let (_, real) = shim_parts(file);
    ((*(*real).pMethods).xUnlock.unwrap())(real, level)
}

unsafe extern "C" fn shim_check_reserved_lock(
    file: *mut ffi::sqlite3_file,
    out: *mut c_int,
) -> c_int {
    let (_, real) = shim_parts(file);
    ((*(*real).pMethods).xCheckReservedLock.unwrap())(real, out)
}

unsafe extern "C" fn shim_file_control(
    file: *mut ffi::sqlite3_file,
    op: c_int,
    arg: *mut c_void,
) -> c_int {
    let (_, real) = shim_parts(file);
    ((*(*real).pMethods).xFileControl.unwrap())(real, op, arg)
}

unsafe extern "C" fn shim_sector_size(file: *mut ffi::sqlite3_file) -> c_int {
    let (_, real) = shim_parts(file);
    ((*(*real).pMethods).xSectorSize.unwrap())(real)
}

unsafe extern "C" fn shim_device_characteristics(file: *mut ffi::sqlite3_file) -> c_int {
    let (_, real) = shim_parts(file);
    ((*(*real).pMethods).xDeviceCharacteristics.unwrap())(real)
}

unsafe extern "C" fn shim_shm_map(
    file: *mut ffi::sqlite3_file,
    page: c_int,
    page_size: c_int,
    extend: c_int,
    out: *mut *mut c_void,
) -> c_int {
    let (_, real) = shim_parts(file);
    ((*(*real).pMethods).xShmMap.unwrap())(real, page, page_size, extend, out)
}

unsafe extern "C" fn shim_shm_lock(
    file: *mut ffi::sqlite3_file,
    offset: c_int,
    n: c_int,
    flags: c_int,
) -> c_int {
    let (_, real) = shim_parts(file);
    ((*(*real).pMethods).xShmLock.unwrap())(real, offset, n, flags)
}

unsafe extern "C" fn shim_shm_barrier(file: *mut ffi::sqlite3_file) {
    let (_, real) = shim_parts(file);
    ((*(*real).pMethods).xShmBarrier.unwrap())(real);
}

unsafe extern "C" fn shim_shm_unmap(file: *mut ffi::sqlite3_file, delete: c_int) -> c_int {
    let (_, real) = shim_parts(file);
    ((*(*real).pMethods).xShmUnmap.unwrap())(real, delete)
}

/// Report "no memory map available", which the VFS contract permits. This
/// forces every page read through `xRead` so read faults are observable.
unsafe extern "C" fn shim_fetch(
    _file: *mut ffi::sqlite3_file,
    _offset: ffi::sqlite3_int64,
    _amt: c_int,
    out: *mut *mut c_void,
) -> c_int {
    *out = std::ptr::null_mut();
    ffi::SQLITE_OK
}

unsafe extern "C" fn shim_unfetch(
    _file: *mut ffi::sqlite3_file,
    _offset: ffi::sqlite3_int64,
    _page: *mut c_void,
) -> c_int {
    ffi::SQLITE_OK
}

unsafe extern "C" fn shim_delete(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    sync_dir: c_int,
) -> c_int {
    let real = real_vfs();
    ((*real).xDelete.unwrap())(real, name, sync_dir)
}

unsafe extern "C" fn shim_access(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    flags: c_int,
    out: *mut c_int,
) -> c_int {
    let real = real_vfs();
    ((*real).xAccess.unwrap())(real, name, flags, out)
}

unsafe extern "C" fn shim_full_pathname(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    out_len: c_int,
    out: *mut c_char,
) -> c_int {
    let real = real_vfs();
    ((*real).xFullPathname.unwrap())(real, name, out_len, out)
}

unsafe extern "C" fn shim_dlopen(_vfs: *mut ffi::sqlite3_vfs, name: *const c_char) -> *mut c_void {
    let real = real_vfs();
    ((*real).xDlOpen.unwrap())(real, name)
}

unsafe extern "C" fn shim_dlerror(_vfs: *mut ffi::sqlite3_vfs, len: c_int, msg: *mut c_char) {
    let real = real_vfs();
    ((*real).xDlError.unwrap())(real, len, msg);
}

unsafe extern "C" fn shim_dlsym(
    _vfs: *mut ffi::sqlite3_vfs,
    handle: *mut c_void,
    symbol: *const c_char,
) -> Option<unsafe extern "C" fn(*mut ffi::sqlite3_vfs, *mut c_void, *const c_char)> {
    let real = real_vfs();
    ((*real).xDlSym.unwrap())(real, handle, symbol)
}

unsafe extern "C" fn shim_dlclose(_vfs: *mut ffi::sqlite3_vfs, handle: *mut c_void) {
    let real = real_vfs();
    ((*real).xDlClose.unwrap())(real, handle);
}

unsafe extern "C" fn shim_randomness(
    _vfs: *mut ffi::sqlite3_vfs,
    n: c_int,
    out: *mut c_char,
) -> c_int {
    let real = real_vfs();
    ((*real).xRandomness.unwrap())(real, n, out)
}

unsafe extern "C" fn shim_sleep(_vfs: *mut ffi::sqlite3_vfs, micros: c_int) -> c_int {
    let real = real_vfs();
    ((*real).xSleep.unwrap())(real, micros)
}

unsafe extern "C" fn shim_current_time(_vfs: *mut ffi::sqlite3_vfs, out: *mut f64) -> c_int {
    let real = real_vfs();
    ((*real).xCurrentTime.unwrap())(real, out)
}

unsafe extern "C" fn shim_get_last_error(
    _vfs: *mut ffi::sqlite3_vfs,
    len: c_int,
    msg: *mut c_char,
) -> c_int {
    let real = real_vfs();
    ((*real).xGetLastError.unwrap())(real, len, msg)
}

unsafe extern "C" fn shim_current_time_int64(
    _vfs: *mut ffi::sqlite3_vfs,
    out: *mut ffi::sqlite3_int64,
) -> c_int {
    let real = real_vfs();
    ((*real).xCurrentTimeInt64.unwrap())(real, out)
}

unsafe extern "C" fn shim_set_system_call(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    call: ffi::sqlite3_syscall_ptr,
) -> c_int {
    let real = real_vfs();
    ((*real).xSetSystemCall.unwrap())(real, name, call)
}

unsafe extern "C" fn shim_get_system_call(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
) -> ffi::sqlite3_syscall_ptr {
    let real = real_vfs();
    ((*real).xGetSystemCall.unwrap())(real, name)
}

unsafe extern "C" fn shim_next_system_call(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
) -> *const c_char {
    let real = real_vfs();
    ((*real).xNextSystemCall.unwrap())(real, name)
}
