use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{OmError, OmResult};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub default: DefaultSection,
    #[serde(default)]
    pub search: SearchSection,
    #[serde(default)]
    pub memory: MemorySection,
    #[serde(default)]
    pub index: IndexSection,
    #[serde(default)]
    pub watch: WatchSection,
    #[serde(default)]
    pub normalization: NormalizationSection,
    #[serde(default)]
    pub engine: EngineSection,
    #[serde(default)]
    pub spaces: SpacesSection,
    #[serde(default)]
    pub audit: AuditSection,
    #[serde(default)]
    pub merge: MergeSection,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DefaultSection {
    #[serde(default)]
    pub jobs: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchSection {
    #[serde(default = "SearchSection::default_hybrid_alpha")]
    pub hybrid_alpha: f32,
    #[serde(default = "SearchSection::default_max_results")]
    pub max_results: usize,
    #[serde(default = "SearchSection::default_rrf_k")]
    pub rrf_k: u32,
    /// Per-field BM25 weights used by the FTS5 keyword backend. The
    /// backend stores a single FTS5 text column and applies each weight
    /// by repeating the corresponding field at index time; higher weights
    /// boost matches in that field. Defaults bias toward `title` and
    /// `entity_name`.
    #[serde(default)]
    pub field_weights: FieldWeights,
}

/// Per-field BM25 weights, in the order expected by the FTS5 backend:
/// `title`, `text`, `summary`, `concepts`, `source_files`,
/// `source_kind`, `entity_type`, `entity_name`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldWeights {
    #[serde(default = "FieldWeights::default_title")]
    pub title: f32,
    #[serde(default = "FieldWeights::default_text")]
    pub text: f32,
    #[serde(default = "FieldWeights::default_summary")]
    pub summary: f32,
    #[serde(default = "FieldWeights::default_concepts")]
    pub concepts: f32,
    #[serde(default = "FieldWeights::default_source_files")]
    pub source_files: f32,
    #[serde(default = "FieldWeights::default_source_kind")]
    pub source_kind: f32,
    #[serde(default = "FieldWeights::default_entity_type")]
    pub entity_type: f32,
    #[serde(default = "FieldWeights::default_entity_name")]
    pub entity_name: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySection {
    #[serde(default = "MemorySection::default_decay_rate")]
    pub decay_rate: f64,
    #[serde(default = "MemorySection::default_consolidation_interval")]
    pub consolidation_interval: u64,
    #[serde(default = "MemorySection::default_dedup_threshold")]
    pub dedup_threshold: f32,
    #[serde(default = "MemorySection::default_prune_floor")]
    pub prune_floor: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexSection {
    #[serde(default = "IndexSection::default_chunk_size")]
    pub chunk_size: usize,
    #[serde(default = "IndexSection::default_max_chars")]
    pub max_chars: usize,
}

/// Filesystem-watcher tuning. Read by `openmemory-watch` when the
/// optional `watch` feature is enabled and `openmemory watch` is
/// invoked. Sensible defaults — most users never touch this section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchSection {
    /// Quiet window before debounced events fire, in milliseconds.
    /// Lower = more responsive, higher = fewer redundant re-indexes.
    #[serde(default = "WatchSection::default_debounce_ms")]
    pub debounce_ms: u64,
    /// File extensions (without leading dot) the watcher considers
    /// observation-shaped text. Empty list defaults to a curated set
    /// at construction time inside `openmemory-watch`.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Skip files larger than this many bytes. Defaults to 10 MiB —
    /// big enough for prose / code, small enough to keep BLAKE3 +
    /// indexing snappy on an editor save loop.
    #[serde(default = "WatchSection::default_max_size")]
    pub max_size: u64,
}

/// Write-behind context-engine tuning. Read by `openmemory-engine` when
/// an ingestion surface (the MCP server, `openmemory ingest`) routes
/// writes through the sharded engine instead of direct `remember`
/// transactions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // config flags, not state-machine state
pub struct EngineSection {
    /// Route MCP `remember` calls through the engine. Off by default;
    /// scriptable single writes gain nothing from write-behind.
    #[serde(default = "EngineSection::default_enabled")]
    pub enabled: bool,
    /// Storage domains: independent SQLite store families committed by
    /// parallel writers, with entities hash-routed by name. `1` (the
    /// default) keeps the classic single-store layout. The count is
    /// pinned per profile on first partitioned open; changing it later
    /// requires an explicit migration. Must divide `shards`.
    #[serde(default = "EngineSection::default_domains")]
    pub domains: usize,
    /// Shard count. Submissions hash by entity name.
    #[serde(default = "EngineSection::default_shards")]
    pub shards: usize,
    /// Epoch length in milliseconds: how often idle shards are drained.
    #[serde(default = "EngineSection::default_flush_interval_ms")]
    pub flush_interval_ms: u64,
    /// Per-shard queue capacity; submission blocks (backpressure) when
    /// a shard is full.
    #[serde(default = "EngineSection::default_shard_capacity")]
    pub shard_capacity: usize,
    /// Background flusher threads.
    #[serde(default = "EngineSection::default_flush_threads")]
    pub flush_threads: usize,
    /// Wait for SQLite durability before acknowledging an MCP remember
    /// call (read-your-writes; costs up to one epoch). Per-call
    /// `durable: false` opts out.
    #[serde(default = "EngineSection::default_durable_ack")]
    pub durable_ack: bool,
    /// Run fuzzy entity-name normalization on drained batches.
    #[serde(default = "EngineSection::default_normalize")]
    pub normalize: bool,
    /// Keep per-shard crash-recovery journals (under
    /// `<data_dir>/engine-journal/`) so acknowledged-but-unflushed
    /// writes survive a crash.
    #[serde(default = "EngineSection::default_journal")]
    pub journal: bool,
    /// Maintenance cadence in milliseconds: WAL checkpoints, deferred
    /// search-index persistence, and journal reclamation run on this
    /// tick (the engine moves SQLite's auto-checkpoint out of commit
    /// paths onto it).
    #[serde(default = "EngineSection::default_checkpoint_interval_ms")]
    pub checkpoint_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizationSection {
    #[serde(default = "NormalizationSection::default_enabled")]
    pub enabled: bool,
    #[serde(default = "NormalizationSection::default_auto_merge_threshold")]
    pub auto_merge_threshold: f64,
    #[serde(default = "NormalizationSection::default_flag_threshold")]
    pub flag_threshold: f64,
    #[serde(default = "NormalizationSection::default_max_candidates")]
    pub max_candidates: usize,
}

/// Semantic-space runtime bounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpacesSection {
    #[serde(default = "SpacesSection::default_enabled")]
    pub enabled: bool,
    #[serde(default = "SpacesSection::default_max_read_set")]
    pub max_read_set: usize,
    #[serde(default = "SpacesSection::default_max_open_spaces")]
    pub max_open_spaces: usize,
    #[serde(default = "SpacesSection::default_idle_close_secs")]
    pub idle_close_secs: u64,
}

/// Audited semantic-mutation limits and retention.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditSection {
    #[serde(default = "AuditSection::default_enabled")]
    pub enabled: bool,
    #[serde(default = "AuditSection::default_rejected_payload_ttl_days")]
    pub rejected_payload_ttl_days: u32,
    #[serde(default = "AuditSection::default_max_payload_bytes")]
    pub max_payload_bytes: usize,
}

/// Directional material-merge safety and retention bounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeSection {
    #[serde(default = "MergeSection::default_enabled")]
    pub enabled: bool,
    #[serde(default = "MergeSection::default_staging_disk_multiplier")]
    pub staging_disk_multiplier: f32,
    #[serde(default = "MergeSection::default_backup_retention_days")]
    pub backup_retention_days: u32,
    #[serde(default = "MergeSection::default_confirmation_ttl_secs")]
    pub confirmation_ttl_secs: u64,
}

impl Config {
    pub fn home_dir() -> OmResult<PathBuf> {
        if let Ok(v) = std::env::var("OPENMEMORY_HOME") {
            return Ok(PathBuf::from(v));
        }
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map_err(|_| OmError::Config("cannot determine home directory".into()))?;
        Ok(PathBuf::from(home).join(".openmemory"))
    }

    pub fn config_path() -> OmResult<PathBuf> {
        Ok(Self::home_dir()?.join("config.toml"))
    }

    pub fn data_dir(profile: &str) -> OmResult<PathBuf> {
        Ok(Self::home_dir()?.join("data").join(profile))
    }

    /// Shared model cache directory (`<home>/models/`). ONNX model
    /// files live here, shared across profiles.
    pub fn models_dir() -> OmResult<PathBuf> {
        Ok(Self::home_dir()?.join("models"))
    }

    /// Shared ONNX Runtime install directory (`<home>/runtime/`). The
    /// platform-matched `libonnxruntime` shared library is unpacked
    /// here under `onnxruntime-<version>/lib/`. Shared across profiles
    /// and resolved by the CLI at startup to set `ORT_DYLIB_PATH`.
    pub fn runtime_dir() -> OmResult<PathBuf> {
        Ok(Self::home_dir()?.join("runtime"))
    }

    pub fn load() -> OmResult<Self> {
        Self::load_from(Self::config_path()?)
    }

    pub fn load_from(path: impl AsRef<Path>) -> OmResult<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&content).map_err(|e| OmError::Config(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> OmResult<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self).map_err(|e| OmError::Config(e.to_string()))?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn validate(&self) -> OmResult<()> {
        if !(0.0..=1.0).contains(&self.search.hybrid_alpha) {
            return Err(OmError::Config(format!(
                "search.hybrid_alpha ({}) must be between 0.0 and 1.0",
                self.search.hybrid_alpha
            )));
        }
        if self.search.max_results == 0 {
            return Err(OmError::Config(
                "search.max_results must be greater than 0".into(),
            ));
        }
        for (name, weight) in self.search.field_weights.named() {
            if !weight.is_finite() || weight < 0.0 {
                return Err(OmError::Config(format!(
                    "search.field_weights.{name} ({weight}) must be finite and non-negative"
                )));
            }
        }
        if self.memory.decay_rate < 0.0 {
            return Err(OmError::Config(
                "memory.decay_rate must be non-negative".into(),
            ));
        }
        if self.index.chunk_size == 0 {
            return Err(OmError::Config(
                "index.chunk_size must be greater than 0".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.normalization.auto_merge_threshold) {
            return Err(OmError::Config(format!(
                "normalization.auto_merge_threshold ({}) must be between 0.0 and 1.0",
                self.normalization.auto_merge_threshold
            )));
        }
        if !(0.0..=1.0).contains(&self.normalization.flag_threshold) {
            return Err(OmError::Config(format!(
                "normalization.flag_threshold ({}) must be between 0.0 and 1.0",
                self.normalization.flag_threshold
            )));
        }
        if self.normalization.flag_threshold >= self.normalization.auto_merge_threshold {
            return Err(OmError::Config(format!(
                "normalization.flag_threshold ({}) must be less than auto_merge_threshold ({})",
                self.normalization.flag_threshold, self.normalization.auto_merge_threshold
            )));
        }
        if self.engine.shards == 0 {
            return Err(OmError::Config(
                "engine.shards must be greater than 0".into(),
            ));
        }
        if self.engine.shard_capacity == 0 {
            return Err(OmError::Config(
                "engine.shard_capacity must be greater than 0".into(),
            ));
        }
        if self.engine.flush_threads == 0 {
            return Err(OmError::Config(
                "engine.flush_threads must be greater than 0".into(),
            ));
        }
        if self.engine.flush_interval_ms == 0 {
            return Err(OmError::Config(
                "engine.flush_interval_ms must be greater than 0".into(),
            ));
        }
        if self.engine.domains == 0 {
            return Err(OmError::Config(
                "engine.domains must be greater than 0".into(),
            ));
        }
        if self.engine.shards % self.engine.domains != 0 {
            return Err(OmError::Config(format!(
                "engine.shards ({}) must be a multiple of engine.domains ({})",
                self.engine.shards, self.engine.domains
            )));
        }
        if self.engine.checkpoint_interval_ms == 0 {
            return Err(OmError::Config(
                "engine.checkpoint_interval_ms must be greater than 0".into(),
            ));
        }
        if !(1..=4).contains(&self.spaces.max_read_set) {
            return Err(OmError::Config(
                "spaces.max_read_set must be between 1 and 4".into(),
            ));
        }
        if !(1..=64).contains(&self.spaces.max_open_spaces) {
            return Err(OmError::Config(
                "spaces.max_open_spaces must be between 1 and 64".into(),
            ));
        }
        if self.spaces.idle_close_secs == 0 {
            return Err(OmError::Config(
                "spaces.idle_close_secs must be greater than 0".into(),
            ));
        }
        if self.audit.rejected_payload_ttl_days > 3_650 {
            return Err(OmError::Config(
                "audit.rejected_payload_ttl_days cannot exceed 3650".into(),
            ));
        }
        if !(1..=4 * 1_024 * 1_024).contains(&self.audit.max_payload_bytes) {
            return Err(OmError::Config(
                "audit.max_payload_bytes must be between 1 and 4194304".into(),
            ));
        }
        if !self.merge.staging_disk_multiplier.is_finite()
            || !(1.0..=2.2).contains(&self.merge.staging_disk_multiplier)
        {
            return Err(OmError::Config(
                "merge.staging_disk_multiplier must be finite and between 1.0 and 2.2".into(),
            ));
        }
        if self.merge.backup_retention_days > 365 {
            return Err(OmError::Config(
                "merge.backup_retention_days cannot exceed 365".into(),
            ));
        }
        if !(1..=3_600).contains(&self.merge.confirmation_ttl_secs) {
            return Err(OmError::Config(
                "merge.confirmation_ttl_secs must be between 1 and 3600".into(),
            ));
        }
        Ok(())
    }

    pub fn num_jobs(&self) -> usize {
        if self.default.jobs == 0 {
            std::thread::available_parallelism().map_or(4, std::num::NonZero::get)
        } else {
            self.default.jobs
        }
    }
}

impl SearchSection {
    fn default_hybrid_alpha() -> f32 {
        0.7
    }
    fn default_max_results() -> usize {
        10
    }
    fn default_rrf_k() -> u32 {
        60
    }
}

impl Default for SearchSection {
    fn default() -> Self {
        Self {
            hybrid_alpha: Self::default_hybrid_alpha(),
            max_results: Self::default_max_results(),
            rrf_k: Self::default_rrf_k(),
            field_weights: FieldWeights::default(),
        }
    }
}

impl FieldWeights {
    fn default_title() -> f32 {
        5.0
    }
    fn default_text() -> f32 {
        1.0
    }
    fn default_summary() -> f32 {
        2.0
    }
    fn default_concepts() -> f32 {
        2.0
    }
    fn default_source_files() -> f32 {
        2.0
    }
    fn default_source_kind() -> f32 {
        0.5
    }
    fn default_entity_type() -> f32 {
        0.5
    }
    fn default_entity_name() -> f32 {
        4.0
    }

    /// Return the per-field weights in the FTS5 payload order:
    /// `title`, `text`, `summary`, `concepts`, `source_files`,
    /// `source_kind`, `entity_type`, `entity_name`.
    #[must_use]
    pub fn as_array(&self) -> [f32; 8] {
        [
            self.title,
            self.text,
            self.summary,
            self.concepts,
            self.source_files,
            self.source_kind,
            self.entity_type,
            self.entity_name,
        ]
    }

    /// Return `(name, weight)` pairs in the same order as [`Self::as_array`].
    #[must_use]
    pub fn named(&self) -> [(&'static str, f32); 8] {
        [
            ("title", self.title),
            ("text", self.text),
            ("summary", self.summary),
            ("concepts", self.concepts),
            ("source_files", self.source_files),
            ("source_kind", self.source_kind),
            ("entity_type", self.entity_type),
            ("entity_name", self.entity_name),
        ]
    }
}

impl Default for FieldWeights {
    fn default() -> Self {
        Self {
            title: Self::default_title(),
            text: Self::default_text(),
            summary: Self::default_summary(),
            concepts: Self::default_concepts(),
            source_files: Self::default_source_files(),
            source_kind: Self::default_source_kind(),
            entity_type: Self::default_entity_type(),
            entity_name: Self::default_entity_name(),
        }
    }
}

impl MemorySection {
    fn default_decay_rate() -> f64 {
        0.01
    }
    fn default_consolidation_interval() -> u64 {
        1800
    }
    fn default_dedup_threshold() -> f32 {
        0.95
    }
    fn default_prune_floor() -> f32 {
        0.05
    }
}

impl Default for MemorySection {
    fn default() -> Self {
        Self {
            decay_rate: Self::default_decay_rate(),
            consolidation_interval: Self::default_consolidation_interval(),
            dedup_threshold: Self::default_dedup_threshold(),
            prune_floor: Self::default_prune_floor(),
        }
    }
}

impl IndexSection {
    fn default_chunk_size() -> usize {
        512
    }
    fn default_max_chars() -> usize {
        100_000
    }
}

impl Default for IndexSection {
    fn default() -> Self {
        Self {
            chunk_size: Self::default_chunk_size(),
            max_chars: Self::default_max_chars(),
        }
    }
}

impl WatchSection {
    fn default_debounce_ms() -> u64 {
        200
    }
    fn default_max_size() -> u64 {
        10 * 1024 * 1024
    }
}

impl Default for WatchSection {
    fn default() -> Self {
        Self {
            debounce_ms: Self::default_debounce_ms(),
            extensions: Vec::new(),
            max_size: Self::default_max_size(),
        }
    }
}

impl EngineSection {
    fn default_enabled() -> bool {
        false
    }
    fn default_domains() -> usize {
        1
    }
    fn default_shards() -> usize {
        32
    }
    fn default_flush_interval_ms() -> u64 {
        20
    }
    fn default_shard_capacity() -> usize {
        4096
    }
    fn default_flush_threads() -> usize {
        2
    }
    fn default_durable_ack() -> bool {
        true
    }
    fn default_normalize() -> bool {
        true
    }
    fn default_journal() -> bool {
        true
    }
    fn default_checkpoint_interval_ms() -> u64 {
        1000
    }
}

impl Default for EngineSection {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            domains: Self::default_domains(),
            shards: Self::default_shards(),
            flush_interval_ms: Self::default_flush_interval_ms(),
            shard_capacity: Self::default_shard_capacity(),
            flush_threads: Self::default_flush_threads(),
            durable_ack: Self::default_durable_ack(),
            normalize: Self::default_normalize(),
            journal: Self::default_journal(),
            checkpoint_interval_ms: Self::default_checkpoint_interval_ms(),
        }
    }
}

impl NormalizationSection {
    fn default_enabled() -> bool {
        true
    }
    fn default_auto_merge_threshold() -> f64 {
        0.95
    }
    fn default_flag_threshold() -> f64 {
        0.85
    }
    fn default_max_candidates() -> usize {
        100
    }
}

impl Default for NormalizationSection {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            auto_merge_threshold: Self::default_auto_merge_threshold(),
            flag_threshold: Self::default_flag_threshold(),
            max_candidates: Self::default_max_candidates(),
        }
    }
}

impl SpacesSection {
    const fn default_enabled() -> bool {
        true
    }
    const fn default_max_read_set() -> usize {
        4
    }
    const fn default_max_open_spaces() -> usize {
        8
    }
    const fn default_idle_close_secs() -> u64 {
        300
    }
}

impl Default for SpacesSection {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            max_read_set: Self::default_max_read_set(),
            max_open_spaces: Self::default_max_open_spaces(),
            idle_close_secs: Self::default_idle_close_secs(),
        }
    }
}

impl AuditSection {
    const fn default_enabled() -> bool {
        true
    }
    const fn default_rejected_payload_ttl_days() -> u32 {
        30
    }
    const fn default_max_payload_bytes() -> usize {
        1024 * 1024
    }
}

impl Default for AuditSection {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            rejected_payload_ttl_days: Self::default_rejected_payload_ttl_days(),
            max_payload_bytes: Self::default_max_payload_bytes(),
        }
    }
}

impl MergeSection {
    const fn default_enabled() -> bool {
        true
    }
    const fn default_staging_disk_multiplier() -> f32 {
        2.2
    }
    const fn default_backup_retention_days() -> u32 {
        7
    }
    const fn default_confirmation_ttl_secs() -> u64 {
        900
    }
}

impl Default for MergeSection {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            staging_disk_multiplier: Self::default_staging_disk_multiplier(),
            backup_retention_days: Self::default_backup_retention_days(),
            confirmation_ttl_secs: Self::default_confirmation_ttl_secs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_validates() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn roundtrip_toml() {
        let config = Config::default();
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.search.max_results, config.search.max_results);
        assert!((deserialized.memory.decay_rate - config.memory.decay_rate).abs() < f64::EPSILON);
    }

    #[test]
    fn load_from_nonexistent_returns_default() {
        let config = Config::load_from("/nonexistent/config.toml").unwrap();
        assert_eq!(config.search.max_results, 10);
    }

    #[test]
    fn load_from_valid_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[search]
max_results = 25
hybrid_alpha = 0.5

[memory]
decay_rate = 0.02
"#,
        )
        .unwrap();

        let config = Config::load_from(&path).unwrap();
        assert_eq!(config.search.max_results, 25);
        assert!((config.search.hybrid_alpha - 0.5).abs() < f32::EPSILON);
        assert!((config.memory.decay_rate - 0.02).abs() < f64::EPSILON);
    }

    #[test]
    fn save_and_reload() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("sub").join("config.toml");

        let mut config = Config::default();
        config.search.max_results = 42;
        config.save(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.search.max_results, 42);
    }

    #[test]
    fn validate_rejects_bad_alpha() {
        let mut config = Config::default();
        config.search.hybrid_alpha = 1.5;
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_rejects_bad_field_weight() {
        let mut config = Config::default();
        config.search.field_weights.summary = -1.0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("search.field_weights.summary"));
    }

    #[test]
    fn field_weights_array_includes_summary() {
        let weights = FieldWeights::default().as_array();
        let expected = [5.0, 1.0, 2.0, 2.0, 2.0, 0.5, 0.5, 4.0];
        for (actual, expected) in weights.iter().zip(expected) {
            assert!((actual - expected).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn validate_rejects_negative_decay() {
        let mut config = Config::default();
        config.memory.decay_rate = -0.1;
        assert!(config.validate().is_err());
    }

    #[test]
    fn engine_section_defaults() {
        let config = Config::default();
        assert!(!config.engine.enabled, "engine is opt-in");
        assert_eq!(config.engine.domains, 1, "partitioning is opt-in");
        assert_eq!(config.engine.checkpoint_interval_ms, 1000);
        assert_eq!(config.engine.shards, 32);
        assert_eq!(config.engine.flush_interval_ms, 20);
        assert_eq!(config.engine.shard_capacity, 4096);
        assert_eq!(config.engine.flush_threads, 2);
        assert!(config.engine.durable_ack);
        assert!(config.engine.normalize);
        assert!(config.engine.journal);
    }

    #[test]
    fn production_space_sections_default_and_roundtrip() {
        let config = Config::default();
        assert!(config.spaces.enabled);
        assert_eq!(config.spaces.max_read_set, 4);
        assert_eq!(config.spaces.max_open_spaces, 8);
        assert!(config.audit.enabled);
        assert_eq!(config.audit.max_payload_bytes, 1024 * 1024);
        assert!(config.merge.enabled);
        assert!((config.merge.staging_disk_multiplier - 2.2).abs() < f32::EPSILON);

        let encoded = toml::to_string(&config).unwrap();
        let decoded: Config = toml::from_str(&encoded).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded.merge.confirmation_ttl_secs, 900);
    }

    #[test]
    fn production_space_sections_enforce_hard_caps() {
        let mut config = Config::default();
        config.spaces.max_read_set = 5;
        assert!(config.validate().is_err());
        config = Config::default();
        config.spaces.max_open_spaces = 65;
        assert!(config.validate().is_err());
        config = Config::default();
        config.audit.max_payload_bytes = 4 * 1024 * 1024 + 1;
        assert!(config.validate().is_err());
        config = Config::default();
        config.merge.staging_disk_multiplier = f32::NAN;
        assert!(config.validate().is_err());
    }

    #[test]
    fn engine_section_parses_from_toml() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[engine]
enabled = true
shards = 8
flush_interval_ms = 50
durable_ack = false
"#,
        )
        .unwrap();
        let config = Config::load_from(&path).unwrap();
        assert!(config.engine.enabled);
        assert_eq!(config.engine.shards, 8);
        assert_eq!(config.engine.flush_interval_ms, 50);
        assert!(!config.engine.durable_ack);
        // Unset keys keep their defaults.
        assert_eq!(config.engine.shard_capacity, 4096);
    }

    #[test]
    fn validate_rejects_zero_engine_knobs() {
        for breakage in [
            |c: &mut Config| c.engine.shards = 0,
            |c: &mut Config| c.engine.shard_capacity = 0,
            |c: &mut Config| c.engine.flush_threads = 0,
            |c: &mut Config| c.engine.flush_interval_ms = 0,
            |c: &mut Config| c.engine.domains = 0,
            |c: &mut Config| c.engine.checkpoint_interval_ms = 0,
        ] {
            let mut config = Config::default();
            breakage(&mut config);
            assert!(config.validate().is_err());
        }
    }

    #[test]
    fn validate_rejects_indivisible_domain_count() {
        let mut config = Config::default();
        config.engine.domains = 5; // 32 shards % 5 != 0
        assert!(config.validate().is_err());
        config.engine.domains = 4;
        config.validate().unwrap();
    }

    #[test]
    fn validate_rejects_zero_chunk_size() {
        let mut config = Config::default();
        config.index.chunk_size = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn home_and_data_dir_respect_env_override() {
        // Merged into a single test so the set/remove pair cannot race
        // with another test reading the same env var under cargo's
        // default parallel test execution.
        std::env::set_var("OPENMEMORY_HOME", "/tmp/om-test");
        let home = Config::home_dir().unwrap();
        assert_eq!(home, PathBuf::from("/tmp/om-test"));
        let data = Config::data_dir("myprofile").unwrap();
        assert_eq!(data, PathBuf::from("/tmp/om-test/data/myprofile"));
        std::env::remove_var("OPENMEMORY_HOME");
    }

    #[test]
    fn num_jobs_explicit() {
        let mut config = Config::default();
        config.default.jobs = 8;
        assert_eq!(config.num_jobs(), 8);
    }

    #[test]
    fn num_jobs_auto() {
        let config = Config::default();
        assert!(config.num_jobs() >= 1);
    }

    #[test]
    fn validate_rejects_bad_normalization_thresholds() {
        let mut config = Config::default();
        config.normalization.flag_threshold = 0.96;
        config.normalization.auto_merge_threshold = 0.95;
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_rejects_normalization_threshold_out_of_range() {
        let mut config = Config::default();
        config.normalization.auto_merge_threshold = 1.5;
        assert!(config.validate().is_err());
    }

    #[test]
    fn normalization_toml_round_trip() {
        let mut config = Config::default();
        config.normalization.enabled = false;
        config.normalization.auto_merge_threshold = 0.90;
        config.normalization.flag_threshold = 0.80;
        config.normalization.max_candidates = 50;

        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert!(!deserialized.normalization.enabled);
        assert!((deserialized.normalization.auto_merge_threshold - 0.90).abs() < f64::EPSILON);
        assert!((deserialized.normalization.flag_threshold - 0.80).abs() < f64::EPSILON);
        assert_eq!(deserialized.normalization.max_candidates, 50);
    }
}
