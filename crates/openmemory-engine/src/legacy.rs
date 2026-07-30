//! Fixed personal-global compatibility opening.
//!
//! Daemon-less CLI and MCP processes cannot create product/team authority.
//! They use this narrow legacy ceremony: one persisted compatibility `SpaceId`
//! and one shared advisory lifetime lock at the profile root.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use fs4::{FileExt, TryLockError};
use openmemory_core::space::SpaceId;
use openmemory_graph::{MemoryError, MemoryResult};
use rusqlite::{OpenFlags, OptionalExtension as _};

pub(crate) const LEGACY_ID_FILE: &str = ".space-id";
pub(crate) const LEGACY_LOCK_FILE: &str = ".personal-global.lock";

#[derive(Debug)]
pub(crate) struct LegacySpaceLock {
    file: File,
    path: PathBuf,
}

impl LegacySpaceLock {
    pub(crate) fn acquire_shared(profile_root: &Path) -> MemoryResult<Self> {
        std::fs::create_dir_all(profile_root)?;
        let path = profile_root.join(LEGACY_LOCK_FILE);
        if matches!(std::fs::symlink_metadata(&path), Ok(metadata) if metadata.file_type().is_symlink())
        {
            return Err(MemoryError::InvalidInput(
                "legacy lifetime lock is a symlink".to_owned(),
            ));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            // A pre-existing lock file is an identity boundary; never erase it.
            .truncate(false)
            .open(&path)?;
        match FileExt::try_lock_shared(&file) {
            Ok(()) => Ok(Self { file, path }),
            Err(TryLockError::WouldBlock) => Err(MemoryError::InvalidInput(
                "space_busy: legacy profile is held exclusively".to_owned(),
            )),
            Err(TryLockError::Error(error)) => Err(MemoryError::Io(error)),
        }
    }

    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LegacySpaceLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub(crate) fn read_or_create_legacy_space_id(profile_root: &Path) -> MemoryResult<SpaceId> {
    std::fs::create_dir_all(profile_root)?;
    let path = profile_root.join(LEGACY_ID_FILE);
    match read_id(&path) {
        Ok(id) => Ok(id),
        Err(MemoryError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            let id = existing_graph_space_id(profile_root)?.unwrap_or_else(SpaceId::new);
            match write_new_id(&path, id) {
                Ok(()) => Ok(id),
                Err(MemoryError::Io(error))
                    if error.kind() == std::io::ErrorKind::AlreadyExists =>
                {
                    read_id(&path)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn existing_graph_space_id(profile_root: &Path) -> MemoryResult<Option<SpaceId>> {
    let path = profile_root.join(openmemory_graph::MEMORY_DB_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let conn = rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let has_meta: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'memory_meta'
         )",
        [],
        |row| row.get(0),
    )?;
    if !has_meta {
        return Ok(None);
    }
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM memory_meta WHERE key = 'space_id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    value
        .map(|value| {
            SpaceId::from_str(&value).map_err(|error| MemoryError::InvalidInput(error.to_string()))
        })
        .transpose()
}

fn read_id(path: &Path) -> MemoryResult<SpaceId> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 256 {
        return Err(MemoryError::InvalidInput(
            "legacy space ID file is unsafe".to_owned(),
        ));
    }
    let mut text = String::new();
    File::open(path)?.take(257).read_to_string(&mut text)?;
    if text.len() > 256 {
        return Err(MemoryError::InvalidInput(
            "legacy space ID file exceeds its bound".to_owned(),
        ));
    }
    SpaceId::from_str(text.trim())
        .map_err(|error| MemoryError::InvalidInput(format!("invalid legacy space ID: {error}")))
}

fn write_new_id(path: &Path, id: SpaceId) -> MemoryResult<()> {
    if matches!(std::fs::symlink_metadata(path), Ok(metadata) if metadata.file_type().is_symlink())
    {
        return Err(MemoryError::InvalidInput(
            "legacy space ID file is a symlink".to_owned(),
        ));
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(id.to_string().as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
