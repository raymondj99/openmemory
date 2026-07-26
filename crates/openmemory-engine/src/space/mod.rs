//! Physical roots and runtime handles for semantic memory spaces.

mod handle;
mod layered;
mod manifest;
mod snapshot;

pub use handle::SpaceHandle;
pub use layered::{
    layered_recall, LayeredRecallRequest, LayeredRecallResponse, RecallOrigin, ScopedRecallResult,
    SpaceReadHandle,
};
pub use manifest::{
    resolve_space_root, SpaceManifest, SPACE_MANIFEST_FILE, SPACE_MANIFEST_FORMAT_VERSION,
};
pub use snapshot::{capture_space_snapshot, DomainVersion, SpaceSnapshot};
