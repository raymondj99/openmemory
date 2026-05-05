//! Knowledge-graph tools. Lands in commit 32. Until then this module
//! exposes [`register_all`] so the registry in [`super::registry`] can stay
//! agnostic of how many tools are wired in.

use crate::tools::Entry;

#[allow(clippy::ptr_arg)]
pub(crate) fn register_all(_out: &mut Vec<Entry>) {
    // Tools land in commit 32.
}
