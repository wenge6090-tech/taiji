//! LiluoClient — 归藏 (cognitive warehouse) file-system client.
//!
//! Cognitive assets (Prompts, Truths, Models) are stored as YAML files under
//! `{data_dir}/{type}s/{id}.yaml`.  An `index.yaml` at the root maintains a
//! tag-based reverse index for efficient search.
//!
//! # Directory layout (V22 三层+预留)
//!
//! ```text
//! {data_dir}/
//! ├── index.yaml          # tag → [AssetRef] reverse index (derived, tag_index only)
//! ├── prompts/            # L5 Prompt assets (行为模板)
//! │   ├── prompt-001.yaml
//! │   └── ...
//! ├── truths/             # L4 Truth assets
//! └── models/             # L2 Model assets (预留 — 待连山流型系统接入)
//! ```
//!
//! # Consistency (AGENTS.md §7)
//! - `save_asset()` reads the current version before overwriting (version++).
//! - `index.yaml` is a derived data structure; `build_index()` rebuilds it
//!   from the raw YAML files when corruption is detected.

use crate::infra::error::TaijiError;
use crate::types::agent::PromptAsset;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

// ---------------------------------------------------------------------------
// Asset reference (used in index.yaml)
// ---------------------------------------------------------------------------

/// Lightweight reference to a cognitive asset, used in the tag index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetRef {
    pub id: String,
    #[serde(rename = "type")]
    pub asset_type: String,
    pub layer: u32,
}

// ---------------------------------------------------------------------------
// Cognitive asset types (full data)
// ---------------------------------------------------------------------------

/// Common header fields shared by all cognitive asset types.
///
/// # Serde note
/// `asset_type` is skipped during (de)serialization because the type is
/// already conveyed by the enclosing `CognitiveAsset` enum tag and by
/// the directory structure (`truths/`, `models/`, etc.).  The field is
/// populated programmatically via `CognitiveAsset::asset_type()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetHeader {
    #[serde(skip)]
    pub asset_type: String,
    pub layer: u32,
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub confidence: f64,
    pub version: u32,
}

/// L4 Truth — hard/soft constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruthAsset {
    #[serde(flatten)]
    pub header: AssetHeader,
    pub severity: String, // "Hard" | "Soft"
    // ── TMS 字段（V18 新增；V22 仅保留审计字段） ──
    #[serde(default)]
    pub status: String, // "active" | "retracted" | "stale"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
}

/// L2 Model — Bayesian confidence model (预留层).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAsset {
    #[serde(flatten)]
    pub header: AssetHeader,
    pub alpha: f64,
    pub beta: f64,
}

/// L1 Skill — registered tool skill with usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillAsset {
    #[serde(flatten)]
    pub header: AssetHeader,
    pub tool_name: String,
    pub trigger_pattern: String,
    pub task_type_tags: Vec<String>,
    pub success_count: u64,
    pub fail_count: u64,
}

// ---------------------------------------------------------------------------
// Index data structure
// ---------------------------------------------------------------------------

/// The on-disk index.yaml schema (V22: tag_index only — TMS dependency_index
/// removed).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexData {
    tag_index: HashMap<String, Vec<AssetRef>>,
}

impl IndexData {
    fn empty() -> Self {
        Self {
            tag_index: HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// LiluoClient
// ---------------------------------------------------------------------------

/// File-system-based 理络 (cognitive network) warehouse client.
///
/// # Thread safety
/// `LiluoClient` is `Send + Sync`.  Internal state (`data_dir`) is immutable
/// after construction.
#[derive(Debug)]
pub struct LiluoClient {
    data_dir: PathBuf,
}

impl LiluoClient {
    /// Directory name for each asset type within `data_dir`.
    fn type_dir_name(type_: &str) -> &'static str {
        match type_ {
            "truth" => "truths",
            "model" => "models",
            "skill" => "skills",
            "prompt" => "prompts",
            _ => {
                // Fallback: treat unknown types as prompts (V22 主层)
                tracing::warn!("unknown cognitive asset type: {type_}, defaulting to 'prompts'");
                "prompts"
            }
        }
    }

    // ── Constructors ──────────────────────────────────────────────────

    /// Create a new `LiluoClient`, ensuring the directory structure exists
    /// and the tag index is built or verified.
    ///
    /// # Errors
    /// Returns `TaijiError::IO` if the data directory cannot be created or
    /// the index cannot be built.
    pub async fn new(data_dir: &Path) -> Result<Self, TaijiError> {
        let this = Self {
            data_dir: data_dir.to_path_buf(),
        };
        this.ensure_dirs().await?;
        this.build_index().await?;
        Ok(this)
    }

    /// Create a sparse `LiluoClient` that skips index building.
    ///
    /// Use this variant when the data directory already exists and its
    /// contents have been initialised externally (e.g. by `cmd_init()`).
    /// Operations that rely on the index (`search_by_tags`) will trigger a
    /// lazy rebuild if `index.yaml` is missing or corrupted.
    pub async fn new_sparse(data_dir: &Path) -> Result<Self, TaijiError> {
        let this = Self {
            data_dir: data_dir.to_path_buf(),
        };
        this.ensure_dirs().await?;
        Ok(this)
    }

    /// Create directories for the asset types under `data_dir`
    /// (V22 三层+预留: prompts/ truths/ models/ + skills 统计元数据).
    async fn ensure_dirs(&self) -> Result<(), TaijiError> {
        let dirs = [
            self.data_dir.join("truths"),
            self.data_dir.join("models"),
            self.data_dir.join("skills"),
            self.data_dir.join("prompts"),
        ];
        for dir in &dirs {
            fs::create_dir_all(dir).await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to create directory {:?}: {e}", dir),
                }
            })?;
        }
        Ok(())
    }

    // ── Health check ──────────────────────────────────────────────────

    /// Verify that the data directory exists.
    pub fn health_check(&self) -> Result<(), TaijiError> {
        if self.data_dir.exists() {
            Ok(())
        } else {
            Err(TaijiError::KnowledgeStoreUnavailable {
                context: format!(
                    "理络 data directory does not exist: {:?}",
                    self.data_dir
                ),
            })
        }
    }

    /// Return the knowledge directory path (replaces `collection_name()`).
    pub fn knowledge_dir(&self) -> &Path {
        &self.data_dir
    }

    // ── Asset I/O ─────────────────────────────────────────────────────

    /// Build the file path for an asset given its type and ID.
    fn asset_path(&self, type_: &str, id: &str) -> PathBuf {
        let dir_name = Self::type_dir_name(type_);
        self.data_dir.join(dir_name).join(format!("{id}.yaml"))
    }

    /// Load a single cognitive asset from its YAML file.
    ///
    /// # Errors
    /// Returns `TaijiError::KnowledgeStoreUnavailable` if the file does not
    /// exist or cannot be parsed.
    pub async fn load_asset(&self, type_: &str, id: &str) -> Result<CognitiveAsset, TaijiError> {
        let path = self.asset_path(type_, id);
        let content = fs::read_to_string(&path).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to read asset {:?}: {e}", path),
            }
        })?;
        let asset: CognitiveAsset = serde_yaml::from_str(&content).map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to parse asset {:?}: {e}", path),
            }
        })?;
        Ok(asset)
    }

    /// Save a cognitive asset to its YAML file with version++.
    ///
    /// Before writing, the current version (if any) is read so the new file
    /// carries `version = old_version + 1`.  If no file exists yet, version
    /// starts at 1.
    ///
    /// After writing, the tag index is rebuilt.
    ///
    /// # Errors
    /// Returns `TaijiError::KnowledgeStoreUnavailable` on I/O or serialisation
    /// failure.
    pub async fn save_asset(&self, asset: &mut CognitiveAsset) -> Result<(), TaijiError> {
        let type_ = asset.asset_type();
        let id = asset.id().to_string();

        // ── Version check: read current file if it exists ──
        let path = self.asset_path(&type_, &id);
        if path.exists() {
            let content = fs::read_to_string(&path).await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to read existing asset for version check: {e}"),
                }
            })?;
            // Extract version from the raw YAML without full deserialisation.
            // We use a minimal extractor to avoid requiring the full type.
            if let Ok(existing) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                let current_version = existing
                    .get("version")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                asset.set_version(current_version + 1);
            } else {
                asset.set_version(1);
            }
        } else {
            asset.set_version(1);
        }

        // ── Write YAML ──
        let yaml = serde_yaml::to_string(asset).map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to serialise asset: {e}"),
            }
        })?;

        // Ensure the directory exists (e.g. for first write).
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to create directory {:?}: {e}", parent),
                }
            })?;
        }

        // Atomic-like write via temporary file + rename.
        let tmp_path = path.with_extension("yaml.tmp");
        {
            let mut tmp = fs::File::create(&tmp_path).await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to create temp file: {e}"),
                }
            })?;
            tmp.write_all(yaml.as_bytes()).await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to write temp file: {e}"),
                }
            })?;
            tmp.flush().await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to flush temp file: {e}"),
                }
            })?;
        }
        fs::rename(&tmp_path, &path).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to rename temp file: {e}"),
            }
        })?;

        // Rebuild index after mutation.
        self.build_index().await?;

        Ok(())
    }

    // ── Tag search ────────────────────────────────────────────────────

    /// Search for assets by tags.
    ///
    /// Returns all assets whose tag sets intersect with any of the given tags.
    /// Relies on the `index.yaml` for efficient lookup; triggers a rebuild
    /// if the index is missing.
    pub async fn search_by_tags(&self, tags: &[&str]) -> Result<Vec<AssetRef>, TaijiError> {
        let index = self.load_or_rebuild_index().await?;

        let mut seen = HashSet::new();
        let mut results = Vec::new();

        for tag in tags {
            if let Some(refs) = index.tag_index.get(*tag) {
                for r in refs {
                    if seen.insert(r.id.clone()) {
                        results.push(r.clone());
                    }
                }
            }
        }

        Ok(results)
    }

    /// Load `index.yaml`, rebuilding it from scratch if missing or corrupt.
    async fn load_or_rebuild_index(&self) -> Result<IndexData, TaijiError> {
        let index_path = self.data_dir.join("index.yaml");

        if index_path.exists() {
            match fs::read_to_string(&index_path).await {
                Ok(content) => {
                    if let Ok(index) = serde_yaml::from_str::<IndexData>(&content) {
                        return Ok(index);
                    }
                    tracing::warn!(
                        "index.yaml is corrupt, rebuilding from asset files"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "failed to read index.yaml ({}), rebuilding",
                        e
                    );
                }
            }
        }

        self.build_index().await?;

        // Re-read the freshly built index.
        let content = fs::read_to_string(&index_path).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to read rebuilt index.yaml: {e}"),
            }
        })?;
        serde_yaml::from_str(&content).map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to parse rebuilt index.yaml: {e}"),
            }
        })
    }

    /// Rebuild `index.yaml` by scanning all YAML files in the type
    /// directories and extracting tags from each.
    pub async fn build_index(&self) -> Result<(), TaijiError> {
        let mut index = IndexData::empty();

        for type_ in &["truth", "model", "skill", "prompt"] {
            let dir = self.data_dir.join(Self::type_dir_name(type_));
            if !dir.exists() {
                continue;
            }

            let mut read_dir = fs::read_dir(&dir).await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to read directory {:?}: {e}", dir),
                }
            })?;

            while let Some(entry) = read_dir.next_entry().await.transpose() {
                match entry {
                    Ok(e) => {
                        let path = e.path();
                        if path.extension().is_none_or(|ext| ext != "yaml") {
                            continue;
                        }
                        if path
                            .file_name()
                            .is_none_or(|n| n.to_string_lossy().ends_with(".tmp"))
                        {
                            continue;
                        }

                        // Read file and extract tags + id.
                        match fs::read_to_string(&path).await {
                            Ok(content) => {
                                if let Ok(val) =
                                    serde_yaml::from_str::<serde_yaml::Value>(&content)
                                {
                                    let asset_id = val
                                        .get("id")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("unknown");
                                    let tags = val
                                        .get("tags")
                                        .and_then(|v| v.as_sequence())
                                        .map(|seq| {
                                            seq.iter()
                                                .filter_map(|v| v.as_str().map(String::from))
                                                .collect::<Vec<_>>()
                                        })
                                        .unwrap_or_default();
                                    let layer = val
                                        .get("layer")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0) as u32;

                                    for tag in &tags {
                                        index
                                            .tag_index
                                            .entry(tag.clone())
                                            .or_default()
                                            .push(AssetRef {
                                                id: asset_id.to_string(),
                                                asset_type: type_.to_string(),
                                                layer,
                                            });
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "failed to read {:?} during index build: {e}",
                                    path
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "error reading directory entry during index build: {e}"
                        );
                    }
                }
            }
        }

        // Write index.yaml atomically.
        let index_path = self.data_dir.join("index.yaml");
        let yaml = serde_yaml::to_string(&index).map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to serialise index: {e}"),
            }
        })?;

        let tmp_path = self.data_dir.join("index.yaml.tmp");
        {
            let mut tmp = fs::File::create(&tmp_path).await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to create index temp file: {e}"),
                }
            })?;
            tmp.write_all(yaml.as_bytes()).await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to write index temp file: {e}"),
                }
            })?;
            tmp.flush().await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to flush index temp file: {e}"),
                }
            })?;
        }
        fs::rename(&tmp_path, &index_path).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to rename index file: {e}"),
            }
        })?;

        Ok(())
    }

    // ── Truth I/O (TMS convenience) ────────────────────────────────────

    /// Load a single Truth asset by ID.
    ///
    /// Returns `None` when the asset does not exist (graceful fallback).
    pub async fn load_truth(&self, id: &str) -> Result<Option<TruthAsset>, TaijiError> {
        match self.load_asset("truth", id).await {
            Ok(CognitiveAsset::Truth(t)) => Ok(Some(t)),
            Ok(_) => {
                tracing::warn!("asset '{id}' found in truths/ but has wrong type tag");
                Ok(None)
            }
            Err(e) => {
                if e.to_string().contains("failed to read asset") {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Save a [`TruthAsset`] to the `truths/` directory.
    ///
    /// Thin wrapper around [`save_asset`](Self::save_asset).
    pub async fn save_truth(&self, truth: &mut TruthAsset) -> Result<(), TaijiError> {
        let mut asset = CognitiveAsset::Truth(truth.clone());
        truth.header.asset_type = "truth".into();
        self.save_asset(&mut asset).await?;
        truth.header.version = asset.version();
        Ok(())
    }

    /// Load all Truth assets whose `status == "active"`.
    ///
    /// Skips retracted/stale truths. Returns all active truths for the
    /// ConstraintEngine to load.
    pub async fn load_active_truths(&self) -> Result<Vec<TruthAsset>, TaijiError> {
        let dir = self.data_dir.join("truths");
        let mut truths = Vec::new();
        if !dir.exists() {
            return Ok(truths);
        }
        let mut read_dir = fs::read_dir(&dir).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to read truths directory: {e}"),
            }
        })?;
        while let Some(entry) = read_dir.next_entry().await.transpose() {
            match entry {
                Ok(e) => {
                    let path = e.path();
                    if path.extension().is_none_or(|ext| ext != "yaml") {
                        continue;
                    }
                    if path.file_name().is_none_or(|n| n.to_string_lossy().ends_with(".tmp")) {
                        continue;
                    }
                    match self.load_truth_from_path(&path).await {
                        Ok(Some(truth)) => {
                            if truth.status == "active" || truth.status.is_empty() {
                                truths.push(truth);
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::warn!("failed to load truth {:?}: {e}", path);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("error reading truths directory entry: {e}");
                }
            }
        }
        Ok(truths)
    }

    /// Load a single Truth asset from a specific file path.
    async fn load_truth_from_path(&self, path: &Path) -> Result<Option<TruthAsset>, TaijiError> {
        let content = fs::read_to_string(path).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to read truth file {:?}: {e}", path),
            }
        })?;
        match serde_yaml::from_str::<CognitiveAsset>(&content) {
            Ok(CognitiveAsset::Truth(t)) => Ok(Some(t)),
            _ => {
                // File might be GFM/YAML frontmatter or some other format.
                Ok(None)
            }
        }
    }


    // ── Prompt asset convenience methods ──────────────────────────────

    /// Save a [`PromptAsset`] to the 理络 `prompts/` directory.
    ///
    /// Thin wrapper around [`save_asset`](Self::save_asset).
    pub async fn save_prompt(&self, prompt: &mut PromptAsset) -> Result<(), TaijiError> {
        let mut asset = CognitiveAsset::Prompt(prompt.clone());
        // Reset type override — the enum tag handles serialisation.
        prompt.asset_type = "prompt".into();
        self.save_asset(&mut asset).await?;
        // Sync version back to the caller.
        prompt.version = asset.version();
        Ok(())
    }

    /// Load a [`PromptAsset`] from the 理络 `prompts/` directory by name.
    ///
    /// Returns `None` when no asset with that name exists (as opposed to
    /// returning an error), so callers can gracefully fall back.
    pub async fn load_prompt(&self, name: &str) -> Result<Option<PromptAsset>, TaijiError> {
        match self.load_asset("prompt", name).await {
            Ok(CognitiveAsset::Prompt(p)) => Ok(Some(p)),
            Ok(_) => {
                // Corrupted: found asset but wrong type tag.
                tracing::warn!("asset '{name}' found in prompts/ but has wrong type tag");
                Ok(None)
            }
            Err(e) => {
                if e.to_string().contains("failed to read asset") {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Search for prompt assets by task-type tags.
    ///
    /// Calls the generic [`search_by_tags`](Self::search_by_tags) then loads
    /// only assets whose type is `"prompt"`.
    pub async fn search_prompts(&self, tags: &[&str]) -> Result<Vec<PromptAsset>, TaijiError> {
        let refs = self.search_by_tags(tags).await?;
        let mut prompts = Vec::new();
        for r in &refs {
            if r.asset_type != "prompt" {
                continue;
            }
            if let Ok(CognitiveAsset::Prompt(p)) = self.load_asset(&r.asset_type, &r.id).await {
                prompts.push(p);
            }
        }
        Ok(prompts)
    }
}

// ---------------------------------------------------------------------------
// CognitiveAsset enum (tagged union for serialisation)
// ---------------------------------------------------------------------------

/// A cognitive asset stored in the 理络 warehouse.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CognitiveAsset {
    #[serde(rename = "truth")]
    Truth(TruthAsset),
    #[serde(rename = "model")]
    Model(ModelAsset),
    #[serde(rename = "skill")]
    Skill(SkillAsset),
    #[serde(rename = "prompt")]
    Prompt(PromptAsset),
}

impl CognitiveAsset {
    /// Return the asset type string (`"truth"`, `"model"`,
    /// `"skill"`, `"prompt"`).
    pub fn asset_type(&self) -> String {
        match self {
            CognitiveAsset::Truth(_) => "truth".into(),
            CognitiveAsset::Model(_) => "model".into(),
            CognitiveAsset::Skill(_) => "skill".into(),
            CognitiveAsset::Prompt(_) => "prompt".into(),
        }
    }

    /// Return the asset ID.
    pub fn id(&self) -> &str {
        match self {
            CognitiveAsset::Truth(a) => &a.header.id,
            CognitiveAsset::Model(a) => &a.header.id,
            CognitiveAsset::Skill(a) => &a.header.id,
            CognitiveAsset::Prompt(a) => &a.id,
        }
    }

    /// Return the asset version.
    pub fn version(&self) -> u32 {
        match self {
            CognitiveAsset::Truth(a) => a.header.version,
            CognitiveAsset::Model(a) => a.header.version,
            CognitiveAsset::Skill(a) => a.header.version,
            CognitiveAsset::Prompt(a) => a.version,
        }
    }

    /// Set the asset version.
    pub fn set_version(&mut self, v: u32) {
        match self {
            CognitiveAsset::Truth(a) => a.header.version = v,
            CognitiveAsset::Model(a) => a.header.version = v,
            CognitiveAsset::Skill(a) => a.header.version = v,
            CognitiveAsset::Prompt(a) => a.version = v,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Create a unique temporary directory for test isolation.
    async fn test_dir(name: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("taiji_liluo_test_{name}_{ts}"));
        // Clean up any previous test data.
        let _ = fs::remove_dir_all(&dir).await;
        dir
    }

    /// Clean up test directory.
    async fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir).await;
    }

    #[tokio::test]
    async fn test_new_creates_dirs() {
        let dir = test_dir("new_creates_dirs").await;
        let _client = LiluoClient::new(&dir).await.unwrap();
        assert!(dir.join("truths").exists());
        assert!(dir.join("models").exists());
        assert!(dir.join("skills").exists());
        assert!(dir.join("index.yaml").exists());
        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_health_check_ok() {
        let dir = test_dir("health_check_ok").await;
        let client = LiluoClient::new(&dir).await.unwrap();
        assert!(client.health_check().is_ok());
        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_save_and_load_truth() {
        let dir = test_dir("save_load_truth").await;
        let client = LiluoClient::new(&dir).await.unwrap();

        let mut asset = CognitiveAsset::Truth(TruthAsset {
            header: AssetHeader {
                asset_type: "truth".into(),
                layer: 4,
                id: "truth-001".into(),
                name: "Test Truth".into(),
                description: "A truth for testing".into(),
                tags: vec!["test".into(), "demo".into()],
                confidence: 0.95,
                version: 0, // will be set to 1 on save
            },
            severity: "Hard".into(),
            status: "active".into(),
            justification: None,
        });

        client.save_asset(&mut asset).await.unwrap();
        assert_eq!(asset.version(), 1);

        // Load back
        let loaded = client.load_asset("truth", "truth-001").await.unwrap();
        match loaded {
            CognitiveAsset::Truth(t) => {
                assert_eq!(t.header.id, "truth-001");
                assert_eq!(t.header.version, 1);
                assert_eq!(t.severity, "Hard");
                assert!(t.header.tags.contains(&"test".to_string()));
            }
            _ => panic!("expected Truth asset"),
        }

        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_save_increments_version() {
        let dir = test_dir("save_increments_version").await;
        let client = LiluoClient::new(&dir).await.unwrap();

        let mut asset = CognitiveAsset::Truth(TruthAsset {
            header: AssetHeader {
                asset_type: "truth".into(),
                layer: 4,
                id: "truth-ver".into(),
                name: "Version Test".into(),
                description: "Testing version++".into(),
                tags: vec!["test".into()],
                confidence: 0.9,
                version: 0,
            },
            severity: "Soft".into(),
            status: "active".into(),
            justification: None,
        });

        client.save_asset(&mut asset).await.unwrap();
        assert_eq!(asset.version(), 1);

        client.save_asset(&mut asset).await.unwrap();
        assert_eq!(asset.version(), 2);

        client.save_asset(&mut asset).await.unwrap();
        assert_eq!(asset.version(), 3);

        cleanup(&dir).await;
    }


    #[tokio::test]
    async fn test_search_by_tags() {
        let dir = test_dir("search_tags").await;
        let client = LiluoClient::new(&dir).await.unwrap();

        // Save a truth with tag "math".
        let mut asset = CognitiveAsset::Truth(TruthAsset {
            header: AssetHeader {
                asset_type: "truth".into(),
                layer: 4,
                id: "truth-math".into(),
                name: "Math Truth".into(),
                description: "".into(),
                tags: vec!["math".into(), "logic".into()],
                confidence: 0.9,
                version: 0,
            },
            severity: "Hard".into(),
            status: "active".into(),
            justification: None,
        });
        client.save_asset(&mut asset).await.unwrap();

        // Save a skill with tag "math".
        let mut skill = CognitiveAsset::Skill(SkillAsset {
            header: AssetHeader {
                asset_type: "skill".into(),
                layer: 1,
                id: "skill-math".into(),
                name: "Math Skill".into(),
                description: "".into(),
                tags: vec!["math".into()],
                confidence: 0.8,
                version: 0,
            },
            tool_name: "calculator".into(),
            trigger_pattern: "calc".into(),
            task_type_tags: vec!["math".into()],
            success_count: 5,
            fail_count: 1,
        });
        client.save_asset(&mut skill).await.unwrap();

        // Search by "math" — should get both assets.
        let results = client.search_by_tags(&["math"]).await.unwrap();
        assert_eq!(results.len(), 2);

        // Search by "logic" — should get only the truth.
        let results = client.search_by_tags(&["logic"]).await.unwrap();
        assert_eq!(results.len(), 1);

        // Search by nonexistent tag — should get nothing.
        let results = client
            .search_by_tags(&["nonexistent"])
            .await
            .unwrap();
        assert!(results.is_empty());

        cleanup(&dir).await;
    }



    #[tokio::test]
    async fn test_index_rebuild_on_corruption() {
        let dir = test_dir("index_corruption").await;
        let client = LiluoClient::new(&dir).await.unwrap();

        // Save an asset so the index is populated.
        let mut asset = CognitiveAsset::Truth(TruthAsset {
            header: AssetHeader {
                asset_type: "truth".into(),
                layer: 4,
                id: "truth-rebuild".into(),
                name: "Rebuild Test".into(),
                description: "".into(),
                tags: vec!["rebuild-test".into()],
                confidence: 0.9,
                version: 0,
            },
            severity: "Soft".into(),
            status: "active".into(),
            justification: None,
        });
        client.save_asset(&mut asset).await.unwrap();

        // Corrupt the index.
        let index_path = dir.join("index.yaml");
        let mut f = fs::File::create(&index_path).await.unwrap();
        f.write_all(b"corrupt: [invalid yaml: {{").await.unwrap();
        f.flush().await.unwrap();
        drop(f);

        // Searching should trigger a rebuild and still find the asset.
        let results = client.search_by_tags(&["rebuild-test"]).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "truth-rebuild");

        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_load_nonexistent_asset_returns_error() {
        let dir = test_dir("load_nonexistent").await;
        let client = LiluoClient::new(&dir).await.unwrap();

        let result = client.load_asset("truth", "nonexistent").await;
        assert!(result.is_err());

        cleanup(&dir).await;
    }

    // ── Prompt asset tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_save_and_load_prompt() {
        let dir = test_dir("save_load_prompt").await;
        let client = LiluoClient::new(&dir).await.unwrap();

        let mut prompt = crate::types::agent::PromptAsset::new(
            "orch-fitting",
            "编排拟合提示词",
            "Orchestration mode FittingAgent system prompt",
            "你是概率拟合专家（编排模式）...",
            "FittingAgent",
            crate::types::agent::AgentMode::Orchestration,
            vec!["fitting".into(), "orchestration".into()],
        );

        client.save_prompt(&mut prompt).await.unwrap();
        assert_eq!(prompt.version, 1);

        // Load back via convenience method.
        let loaded = client.load_prompt("orch-fitting").await.unwrap();
        assert!(loaded.is_some());
        let p = loaded.unwrap();
        assert_eq!(p.name, "编排拟合提示词");
        assert_eq!(p.agent_target, "FittingAgent");
        assert_eq!(p.agent_mode, crate::types::agent::AgentMode::Orchestration);
        assert!(p.tags.contains(&"fitting".to_string()));

        // Load nonexistent prompt returns None (not error).
        let missing = client.load_prompt("nonexistent").await.unwrap();
        assert!(missing.is_none());

        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_search_prompts() {
        let dir = test_dir("search_prompts").await;
        let client = LiluoClient::new(&dir).await.unwrap();

        // Save two prompts with overlapping tags.
        let mut p1 = crate::types::agent::PromptAsset::new(
            "exec-fitting",
            "执行拟合提示词",
            "Execution mode FittingAgent prompt",
            "你是执行专家...",
            "FittingAgent",
            crate::types::agent::AgentMode::Execution,
            vec!["fitting".into(), "execution".into()],
        );
        client.save_prompt(&mut p1).await.unwrap();

        let mut p2 = crate::types::agent::PromptAsset::new(
            "exec-verify",
            "执行验证提示词",
            "Execution mode CausalAgent verify prompt",
            "你是因果验证器（执行模式）...",
            "CausalAgent",
            crate::types::agent::AgentMode::Execution,
            vec!["verify".into(), "execution".into()],
        );
        client.save_prompt(&mut p2).await.unwrap();

        // Search by "fitting" — should find only p1.
        let results = client.search_prompts(&["fitting"]).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "exec-fitting");

        // Search by "execution" — should find both.
        let results = client.search_prompts(&["execution"]).await.unwrap();
        assert_eq!(results.len(), 2);

        // Search by nonexistent tag — empty.
        let results = client
            .search_prompts(&["nonexistent"])
            .await
            .unwrap();
        assert!(results.is_empty());

        cleanup(&dir).await;
    }
}
