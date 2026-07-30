//! Versioned, domain-separated, length-framed canonical encoding.

use openmemory_core::space::{RevisionId, SnapshotId, SpaceId};

use crate::hash::{
    ActionStreamHash, CandidateHash, PacketHash, PlanHash, PredictedResultHash, ReceiptHash,
    SemanticHash, SnapshotHash,
};

pub(crate) struct CanonicalHasher {
    inner: blake3::Hasher,
    encoded_len: usize,
}

impl CanonicalHasher {
    pub(crate) fn new(domain: &'static [u8]) -> Self {
        let mut value = Self {
            inner: blake3::Hasher::new(),
            encoded_len: 0,
        };
        value.bytes(b"openmemory/canonical/v1");
        value.bytes(domain);
        value
    }

    pub(crate) fn bytes(&mut self, value: &[u8]) {
        self.inner
            .update(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
        self.inner.update(value);
        self.encoded_len = self
            .encoded_len
            .saturating_add(8)
            .saturating_add(value.len());
    }

    pub(crate) fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    pub(crate) fn bool(&mut self, value: bool) {
        self.inner.update(&[u8::from(value)]);
        self.encoded_len = self.encoded_len.saturating_add(1);
    }

    pub(crate) fn u16(&mut self, value: u16) {
        self.inner.update(&value.to_le_bytes());
        self.encoded_len = self.encoded_len.saturating_add(2);
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.inner.update(&value.to_le_bytes());
        self.encoded_len = self.encoded_len.saturating_add(8);
    }

    pub(crate) fn usize(&mut self, value: usize) {
        self.u64(u64::try_from(value).unwrap_or(u64::MAX));
    }

    pub(crate) fn optional_text(&mut self, value: Option<&str>) {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.text(value);
        }
    }

    pub(crate) fn space_id(&mut self, value: SpaceId) {
        self.text(&value.to_string());
    }

    pub(crate) fn snapshot_id(&mut self, value: SnapshotId) {
        self.text(&value.to_string());
    }

    pub(crate) fn revision_id(&mut self, value: RevisionId) {
        self.text(&value.to_string());
    }

    pub(crate) fn hash(&mut self, value: &[u8; 32]) {
        self.bytes(value);
    }

    pub(crate) const fn encoded_len(&self) -> usize {
        self.encoded_len
    }

    fn finish(self) -> [u8; 32] {
        *self.inner.finalize().as_bytes()
    }

    pub(crate) fn finish_semantic(self) -> SemanticHash {
        SemanticHash::from_bytes(self.finish())
    }

    pub(crate) fn finish_snapshot(self) -> SnapshotHash {
        SnapshotHash::from_bytes(self.finish())
    }

    pub(crate) fn finish_packet(self) -> PacketHash {
        PacketHash::from_bytes(self.finish())
    }

    pub(crate) fn finish_candidate(self) -> CandidateHash {
        CandidateHash::from_bytes(self.finish())
    }

    pub(crate) fn finish_receipt(self) -> ReceiptHash {
        ReceiptHash::from_bytes(self.finish())
    }

    pub(crate) fn finish_action_stream(self) -> ActionStreamHash {
        ActionStreamHash::from_bytes(self.finish())
    }

    pub(crate) fn finish_predicted(self) -> PredictedResultHash {
        PredictedResultHash::from_bytes(self.finish())
    }

    pub(crate) fn finish_plan(self) -> PlanHash {
        PlanHash::from_bytes(self.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_and_domain_separation_prevent_ambiguous_hashes() {
        let mut left = CanonicalHasher::new(b"test/one");
        left.text("ab");
        left.text("c");
        let left = left.finish_semantic();

        let mut right = CanonicalHasher::new(b"test/one");
        right.text("a");
        right.text("bc");
        let right = right.finish_semantic();

        let mut other_domain = CanonicalHasher::new(b"test/two");
        other_domain.text("ab");
        other_domain.text("c");
        let other_domain = other_domain.finish_semantic();

        assert_ne!(left, right);
        assert_ne!(left, other_domain);
    }
}
