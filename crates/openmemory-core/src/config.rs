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
