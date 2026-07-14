//! LiluoClient — 理络 (cognitive network) file-system warehouse.
//!
//! All cognitive assets (Truths, Grids, Models, Skills) are stored as YAML
//! files under `{data_dir}/{type}s/{id}.yaml`.  An `index.yaml` at the root
//! maintains a tag-based reverse index for efficient search.
//!
//! # Directory layout
//!
//! ```text
//! {data_dir}/
//! ├── index.yaml          # tag → [AssetRef] reverse index (derived)
//! ├── truths/             # L4 Truth assets
//! │   ├── truth-001.yaml
//! │   └── ...
//! ├── grids/              # L3 Grid assets (with inline relations)
//! ├── models/             # L2 Model assets (Bayesian)
//! └── skills/             # L1 Skill assets
//! ```
//!
//! # Consistency (AGENTS.md §7)
//! - `save_asset()` reads the current version before overwriting (version++).
//! - `index.yaml` is a derived data structure; `build_index()` rebuilds it
//!   from the raw YAML files when corruption is detected.

use crate::infra::error::TaijiError;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
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
/// the directory structure (`truths/`, `grids/`, etc.).  The field is
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
}

/// L3 Grid — cognitive grid with typed relations to other assets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridAsset {
    #[serde(flatten)]
    pub header: AssetHeader,
    pub relations: Vec<Relation>,
}

/// L2 Model — Bayesian confidence model.
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

/// Typed relation edge between two cognitive assets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub target_id: String,
    pub target_type: String,
    pub relation_type: String,
    pub weight: f64,
    pub interpretation: String,
}

// ---------------------------------------------------------------------------
// Index data structure
// ---------------------------------------------------------------------------

/// The on-disk index.yaml schema.
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
            "grid" => "grids",
            "model" => "models",
            "skill" => "skills",
            _ => {
                // Fallback: treat unknown types as grids
                tracing::warn!("unknown cognitive asset type: {type_}, defaulting to 'grids'");
                "grids"
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

    /// Create directories for all four asset types under `data_dir`.
    async fn ensure_dirs(&self) -> Result<(), TaijiError> {
        let dirs = [
            self.data_dir.join("truths"),
            self.data_dir.join("grids"),
            self.data_dir.join("models"),
            self.data_dir.join("skills"),
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

        for type_ in &["truth", "grid", "model", "skill"] {
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
                        if path.extension().map_or(true, |ext| ext != "yaml") {
                            continue;
                        }
                        if path
                            .file_name()
                            .map_or(true, |n| n.to_string_lossy().ends_with(".tmp"))
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

    // ── Relation traversal (BFS) ──────────────────────────────────────

    /// Traverse relations starting from an asset using BFS, deduplicating
    /// visited nodes to prevent cycles.
    ///
    /// `max_hops` bounds the traversal depth.  Returns a flat list of
    /// all [`Relation`] edges discovered.
    pub async fn traverse_relations(
        &self,
        start_id: &str,
        max_hops: u32,
    ) -> Result<Vec<Relation>, TaijiError> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, u32)> = VecDeque::new();
        let mut edges: Vec<Relation> = Vec::new();

        queue.push_back((start_id.to_string(), 0));
        visited.insert(start_id.to_string());

        while let Some((current_id, depth)) = queue.pop_front() {
            if depth >= max_hops {
                continue;
            }

            // Try loading as grid (only grids carry relations).
            // Also try other types in case they have relations.
            for type_ in &["grid", "truth", "model", "skill"] {
                if let Ok(asset) = self.load_asset(type_, &current_id).await {
                    match asset {
                        CognitiveAsset::Grid(grid) => {
                            for rel in &grid.relations {
                                edges.push(rel.clone());
                                if visited.insert(rel.target_id.clone()) {
                                    queue.push_back((rel.target_id.clone(), depth + 1));
                                }
                            }
                        }
                        _ => {}
                    }
                    break; // Found the asset, no need to try other types
                }
            }
        }

        Ok(edges)
    }

    /// Build reasoning paths from a set of starting assets.
    ///
    /// For each start ID, performs a BFS up to `max_hops` and produces a
    /// [`ReasoningPath`] per start node.
    ///
    /// Uses the existing [`super::super::types::agent::ReasoningPath`] and
    /// [`super::super::types::agent::Chain`] types for compatibility with
    /// the agent system.
    pub async fn build_reasoning_paths(
        &self,
        start_ids: &[String],
        max_hops: u32,
    ) -> Result<Vec<crate::types::agent::ReasoningPath>, TaijiError> {
        let mut paths = Vec::new();

        for start_id in start_ids {
            let edges = self.traverse_relations(start_id, max_hops).await?;

            let chains: Vec<crate::types::agent::Chain> = edges
                .into_iter()
                .map(|rel| crate::types::agent::Chain {
                    source: start_id.clone(),
                    target: rel.target_id,
                    target_type: rel.target_type,
                    relation_type: rel.relation_type,
                    weight: rel.weight,
                    interpretation: rel.interpretation,
                })
                .collect();

            let depth = chains
                .iter()
                .map(|_| 1u32)
                .max()
                .unwrap_or(0)
                .min(max_hops);

            paths.push(crate::types::agent::ReasoningPath {
                source_grid: start_id.clone(),
                chains,
                depth,
                task_type_tags: vec![],
            });
        }

        Ok(paths)
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
    #[serde(rename = "grid")]
    Grid(GridAsset),
    #[serde(rename = "model")]
    Model(ModelAsset),
    #[serde(rename = "skill")]
    Skill(SkillAsset),
}

impl CognitiveAsset {
    /// Return the asset type string (`"truth"`, `"grid"`, `"model"`,
    /// `"skill"`).
    pub fn asset_type(&self) -> String {
        match self {
            CognitiveAsset::Truth(_) => "truth".into(),
            CognitiveAsset::Grid(_) => "grid".into(),
            CognitiveAsset::Model(_) => "model".into(),
            CognitiveAsset::Skill(_) => "skill".into(),
        }
    }

    /// Return the asset ID.
    pub fn id(&self) -> &str {
        match self {
            CognitiveAsset::Truth(a) => &a.header.id,
            CognitiveAsset::Grid(a) => &a.header.id,
            CognitiveAsset::Model(a) => &a.header.id,
            CognitiveAsset::Skill(a) => &a.header.id,
        }
    }

    /// Return the asset version.
    pub fn version(&self) -> u32 {
        match self {
            CognitiveAsset::Truth(a) => a.header.version,
            CognitiveAsset::Grid(a) => a.header.version,
            CognitiveAsset::Model(a) => a.header.version,
            CognitiveAsset::Skill(a) => a.header.version,
        }
    }

    /// Set the asset version.
    pub fn set_version(&mut self, v: u32) {
        match self {
            CognitiveAsset::Truth(a) => a.header.version = v,
            CognitiveAsset::Grid(a) => a.header.version = v,
            CognitiveAsset::Model(a) => a.header.version = v,
            CognitiveAsset::Skill(a) => a.header.version = v,
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
        assert!(dir.join("grids").exists());
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
    async fn test_save_and_load_grid_with_relations() {
        let dir = test_dir("grid_relations").await;
        let client = LiluoClient::new(&dir).await.unwrap();

        let mut grid = CognitiveAsset::Grid(GridAsset {
            header: AssetHeader {
                asset_type: "grid".into(),
                layer: 3,
                id: "grid-001".into(),
                name: "Test Grid".into(),
                description: "Grid with relations".into(),
                tags: vec!["test".into()],
                confidence: 0.8,
                version: 0,
            },
            relations: vec![Relation {
                target_id: "truth-001".into(),
                target_type: "truth".into(),
                relation_type: "causes".into(),
                weight: 0.7,
                interpretation: "leads to".into(),
            }],
        });

        client.save_asset(&mut grid).await.unwrap();

        let loaded = client.load_asset("grid", "grid-001").await.unwrap();
        match loaded {
            CognitiveAsset::Grid(g) => {
                assert_eq!(g.relations.len(), 1);
                assert_eq!(g.relations[0].relation_type, "causes");
            }
            _ => panic!("expected Grid asset"),
        }

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
    async fn test_traverse_relations_bfs() {
        let dir = test_dir("traverse_bfs").await;
        let client = LiluoClient::new(&dir).await.unwrap();

        // Save grid-0 with relation to grid-1.
        let mut g0 = CognitiveAsset::Grid(GridAsset {
            header: AssetHeader {
                asset_type: "grid".into(),
                layer: 3,
                id: "grid-0".into(),
                name: "Root".into(),
                description: "".into(),
                tags: vec![],
                confidence: 0.8,
                version: 0,
            },
            relations: vec![Relation {
                target_id: "grid-1".into(),
                target_type: "grid".into(),
                relation_type: "causes".into(),
                weight: 0.9,
                interpretation: "triggers".into(),
            }],
        });
        client.save_asset(&mut g0).await.unwrap();

        // Save grid-1 with relation to grid-2.
        let mut g1 = CognitiveAsset::Grid(GridAsset {
            header: AssetHeader {
                asset_type: "grid".into(),
                layer: 3,
                id: "grid-1".into(),
                name: "Mid".into(),
                description: "".into(),
                tags: vec![],
                confidence: 0.8,
                version: 0,
            },
            relations: vec![Relation {
                target_id: "grid-2".into(),
                target_type: "grid".into(),
                relation_type: "inhibits".into(),
                weight: -0.5,
                interpretation: "blocks".into(),
            }],
        });
        client.save_asset(&mut g1).await.unwrap();

        // Save grid-2 (leaf).
        let mut g2 = CognitiveAsset::Grid(GridAsset {
            header: AssetHeader {
                asset_type: "grid".into(),
                layer: 3,
                id: "grid-2".into(),
                name: "Leaf".into(),
                description: "".into(),
                tags: vec![],
                confidence: 0.8,
                version: 0,
            },
            relations: vec![],
        });
        client.save_asset(&mut g2).await.unwrap();

        // Traverse from grid-0 with 1 hop — should get only grid-0→grid-1.
        let edges = client
            .traverse_relations("grid-0", 1)
            .await
            .unwrap();
        assert_eq!(edges.len(), 1);

        // Traverse from grid-0 with 2 hops — should get both edges.
        let edges = client
            .traverse_relations("grid-0", 2)
            .await
            .unwrap();
        assert_eq!(edges.len(), 2);

        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_build_reasoning_paths() {
        let dir = test_dir("reasoning_paths").await;
        let client = LiluoClient::new(&dir).await.unwrap();

        // Create a simple chain: grid-a → grid-b
        let mut ga = CognitiveAsset::Grid(GridAsset {
            header: AssetHeader {
                asset_type: "grid".into(),
                layer: 3,
                id: "grid-a".into(),
                name: "Source".into(),
                description: "".into(),
                tags: vec!["test".into()],
                confidence: 0.8,
                version: 0,
            },
            relations: vec![Relation {
                target_id: "grid-b".into(),
                target_type: "grid".into(),
                relation_type: "causes".into(),
                weight: 0.6,
                interpretation: "activates".into(),
            }],
        });
        client.save_asset(&mut ga).await.unwrap();

        let mut gb = CognitiveAsset::Grid(GridAsset {
            header: AssetHeader {
                asset_type: "grid".into(),
                layer: 3,
                id: "grid-b".into(),
                name: "Target".into(),
                description: "".into(),
                tags: vec![],
                confidence: 0.8,
                version: 0,
            },
            relations: vec![],
        });
        client.save_asset(&mut gb).await.unwrap();

        let paths = client
            .build_reasoning_paths(&["grid-a".to_string()], 3)
            .await
            .unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].source_grid, "grid-a");
        assert_eq!(paths[0].chains.len(), 1);
        assert_eq!(paths[0].chains[0].target, "grid-b");

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
}
