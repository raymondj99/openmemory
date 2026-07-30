//! Space-bound engine coordination.
//!
//! Managed memory spaces: durable per-space manifests ([`SpaceManifest`]),
//! the bounded lazy [`SpaceManager`] owning one complete `DomainStore` per
//! space, and rank-interleaving layered recall composition. Snapshots
//! remain a later delivery phase.

#![forbid(unsafe_code)]

mod handle;
mod layered;
mod manifest;
mod snapshot;

pub use handle::{
    validate_space_name, SpaceHandle, SpaceInfo, SpaceManager, MAX_OPEN_SPACES, SPACES_DIR,
};
pub use layered::{interleave_by_rank, LayeredHit, MAX_READ_SPACES};
pub use manifest::{SpaceManifest, SPACE_MANIFEST_FILE};
