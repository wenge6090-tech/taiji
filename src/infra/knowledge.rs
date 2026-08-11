//! LiluoClient — 归藏 (cognitive warehouse) file-system client.
//!
//! Cognitive assets (Prompts, Models, Skills, Verifications) are stored as
//! YAML files under `{data_dir}/{type}s/{id}.yaml`. V38: no `index.yaml` —
//! tag search scans directories on demand (`scan_assets`).
//!
//! # Directory layout (V38：无 index.yaml / 无 truths/)
//!
//! ```text
//! {data_dir}/
//! ├── prompts/            # L5 Prompt assets (行为模板)
//! │   ├── prompt-001.yaml
//! │   └── ...
//! ├── verifications/      # L1 阴轨验证契约（V33 结构化 checks）
//! ├── models/             # L2 Model assets（贝叶斯后验，MVP-3.5 激活）
//! └── skills/             # L1 技能统计元数据
//! ```
//!
//! # Consistency (AGENTS.md §7)
//! - `save_asset()` reads the current version before overwriting (version++).
//! - V38：标签检索实时目录扫描（`scan_assets`），无持久化索引需维护。

use crate::infra::error::TaijiError;
use crate::types::agent::{PromptAsset, VerificationAsset};
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

/// L2 Model — Bayesian confidence model（MVP-3.5 激活，原「预留层」— BCP §6.2/§6.4.1）。
/// 每验证契约一个资产（id 与 verification 同名关联）；Beta-Bernoulli 共轭后验。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAsset {
    #[serde(flatten)]
    pub header: AssetHeader,
    pub alpha: f64,
    pub beta: f64,
}

impl ModelAsset {
    /// 后验均值 μ = α/(α+β)（§6.4.1）。
    pub fn posterior_mean(&self) -> f64 {
        let total = self.alpha + self.beta;
        if total <= 0.0 {
            0.0
        } else {
            self.alpha / total
        }
    }

    /// 后验标准差 σ = √(αβ/((α+β)²·(α+β+1)))（§6.4.1——Beta 分布标准差）。
    /// 低采样时 σ 大（后验宽），驱动决策保守化（不误淘汰）。
    pub fn posterior_sigma(&self) -> f64 {
        let total = self.alpha + self.beta;
        if total <= 0.0 {
            0.0
        } else {
            (self.alpha * self.beta / (total * total * (total + 1.0))).sqrt()
        }
    }

    /// 从人工先验（资产 confidence，§6.3 语义）映射初始化：
    /// α = 1 + k·confidence，β = 1 + k·(1−confidence)（k = prior_strength，§6.4.1）。
    pub fn from_prior(id: &str, name: &str, confidence: f64, prior_strength: f64) -> Self {
        let k = prior_strength.max(0.0);
        let c = confidence.clamp(0.0, 1.0);
        Self {
            header: AssetHeader {
                asset_type: "model".into(),
                layer: 2,
                id: id.to_string(),
                name: name.to_string(),
                description: format!("Bayesian posterior for verification contract {id}"),
                tags: vec!["bayesian".into()],
                confidence,
                version: 1,
            },
            alpha: 1.0 + k * c,
            beta: 1.0 + k * (1.0 - c),
        }
    }
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

/// V38 内存 tag 索引（实时扫描构建，不落盘——替代原 index.yaml schema）。
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
///
/// # 分区（V36，BCP §6.1）
/// - `root_dir` = knowledge 根（构造时传入的 `data_dir`），`model_stats.yaml`
///   恒在根级（跨分区共享，ModelRouter 数据源）。
/// - `data_dir` = 活动目录：根 client 时 = root_dir；分区 client 时 =
///   `root_dir/{model_key}`（`for_model` 派生）。
/// - 分区一致性（§8.3）：一个任务内所有 Agent 使用同一分区——`MetaContext.model`
///   是唯一载体；MetaAgent 按路由结果 `for_model` 检索，DMN 按 pending 的
///   `model_key` 分区回传。
#[derive(Debug)]
pub struct LiluoClient {
    /// knowledge 根目录（构造时传入）——model_stats.yaml 所在层。
    root_dir: PathBuf,
    /// 活动目录：根 client = root_dir；分区 client = root_dir/{model_key}。
    data_dir: PathBuf,
    /// 分区键（None = 根/未分区）。
    partition: Option<String>,
}

impl LiluoClient {
    /// Directory name for each asset type within `data_dir`.
    fn type_dir_name(type_: &str) -> &'static str {
        match type_ {
            "model" => "models",
            "skill" => "skills",
            "prompt" => "prompts",
            "verification" => "verifications",
            _ => {
                // Fallback: treat unknown types as prompts (V22 主层)
                tracing::warn!("unknown cognitive asset type: {type_}, defaulting to 'prompts'");
                "prompts"
            }
        }
    }

    // ── Constructors ──────────────────────────────────────────────────

    /// Create a new `LiluoClient`, ensuring the directory structure exists.
    ///
    /// # Errors
    /// Returns `TaijiError::IO` if the data directory cannot be created.
    pub async fn new(data_dir: &Path) -> Result<Self, TaijiError> {
        let this = Self {
            root_dir: data_dir.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
            partition: None,
        };
        this.ensure_dirs().await?;
        Ok(this)
    }

    /// 派生指定模型分区 client（V36，BCP §6.1）——`data_dir = root/{model_key}`，
    /// root_dir 保持（model_stats 仍根级）。自动创建分区目录 + 资产目录，
    /// 调用方拿到即可检索/写入。
    ///
    /// 从根 client 或分区 client 均可派生（从分区派生会切换到新分区）。
    pub async fn for_model(&self, model_key: &str) -> Result<Self, TaijiError> {
        let partition_dir = self.root_dir.join(model_key);
        let this = Self {
            root_dir: self.root_dir.clone(),
            data_dir: partition_dir,
            partition: Some(model_key.to_string()),
        };
        this.ensure_dirs().await?;
        Ok(this)
    }

    /// 当前分区键（None = 根/未分区）。
    pub fn partition_key(&self) -> Option<&str> {
        self.partition.as_deref()
    }

    /// knowledge 根目录（model_stats.yaml 所在层）。
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    /// Create a sparse `LiluoClient` that skips index building.
    ///
    /// Use this variant when the data directory already exists and its
    /// contents have been initialised externally (e.g. by `cmd_init()`).
    /// Operations that rely on the index (`search_by_tags`) will trigger a
    /// lazy rebuild if `index.yaml` is missing or corrupted.
    pub async fn new_sparse(data_dir: &Path) -> Result<Self, TaijiError> {
        let this = Self {
            root_dir: data_dir.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
            partition: None,
        };
        this.ensure_dirs().await?;
        Ok(this)
    }

    /// Create directories for the asset types under `data_dir`
    /// (V22 三层+预留: models/ skills/ prompts；V33 加 verifications/ 阴轨验证契约层；
    /// V38 移除 truths/ 资产层——L0 检查内置化)。
    async fn ensure_dirs(&self) -> Result<(), TaijiError> {
        let dirs = [
            self.data_dir.join("models"),
            self.data_dir.join("skills"),
            self.data_dir.join("prompts"),
            self.data_dir.join("verifications"),
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

    // ── Model stats（V36 元权重表，根级共享）──────────────────────────

    /// 加载根级 model_stats.yaml（BCP §6.4 元权重表）——`model_key → StatsRow`。
    ///
    /// 文件缺失 → 空表（未采样 = 合法状态，ModelRouter 走默认模型）；文件损坏
    /// → warn + 空表（衍生数据无重建源，按未采样处理——与 index.yaml 损坏重建
    /// 同性质，不阻断检索主流程）。
    pub async fn load_model_stats(
        &self,
    ) -> Result<std::collections::BTreeMap<String, crate::types::agent::ModelStatsRow>, TaijiError> {
        let path = self.root_dir.join("model_stats.yaml");
        match fs::read_to_string(&path).await {
            Ok(content) => match serde_yaml::from_str::<
                std::collections::BTreeMap<String, crate::types::agent::ModelStatsRow>,
            >(&content)
            {
                Ok(stats) => Ok(stats),
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "model_stats.yaml corrupted — treating as empty (no rebuild source)"
                    );
                    Ok(std::collections::BTreeMap::new())
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(std::collections::BTreeMap::new())
            }
            Err(e) => Err(TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to read model_stats {:?}: {e}", path),
            }),
        }
    }

    /// 原子写根级 model_stats.yaml（DMN 单写者调用；TPN 只读）。
    pub async fn save_model_stats(
        &self,
        stats: &std::collections::BTreeMap<String, crate::types::agent::ModelStatsRow>,
    ) -> Result<(), TaijiError> {
        let path = self.root_dir.join("model_stats.yaml");
        let yaml = serde_yaml::to_string(stats).map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to serialise model_stats: {e}"),
            }
        })?;
        let tmp_path = path.with_extension("yaml.tmp");
        {
            let mut tmp = fs::File::create(&tmp_path).await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to create temp file for model_stats: {e}"),
                }
            })?;
            tmp.write_all(yaml.as_bytes()).await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to write temp file for model_stats: {e}"),
                }
            })?;
            tmp.flush().await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to flush temp file for model_stats: {e}"),
                }
            })?;
        }
        fs::rename(&tmp_path, &path).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to rename model_stats temp file: {e}"),
            }
        })?;
        Ok(())
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

        Ok(())
    }

    // ── Tag search ────────────────────────────────────────────────────

    /// Search for assets by tags.
    ///
    /// Returns all assets whose tag sets intersect with any of the given tags.
    /// V38：实时目录扫描（`scan_assets` 内存构建 tag → AssetRef 映射，不落盘）——
    /// 资产量级几十个，扫描毫秒级；省去 index.yaml 的读写与一致性维护。
    pub async fn search_by_tags(&self, tags: &[&str]) -> Result<Vec<AssetRef>, TaijiError> {
        let index = self.scan_assets().await?;

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

    /// 实时扫描所有资产目录，内存构建 tag 反向索引（V38：替代 index.yaml）。
    /// 扫描逻辑继承原 build_index：遍历各资产层的 *.yaml（跳过 .tmp），
    /// 提取 id / tags / layer 建索引。不落盘。
    pub async fn scan_assets(&self) -> Result<IndexData, TaijiError> {
        let mut index = IndexData::empty();

        for type_ in &["model", "skill", "prompt", "verification"] {
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

        Ok(index)
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
            // 逐个加载；失败必须可见（V32 实测：资产 YAML 缺字段时 load_asset
            // 失败被静默吞掉 → MetaAgent 零资产降级 → 编排失效的系统性 bug）。
            match self.load_asset(&r.asset_type, &r.id).await {
                Ok(CognitiveAsset::Prompt(p)) => prompts.push(p),
                Ok(_) => {
                    tracing::warn!(
                        "search_prompts: asset '{}' has wrong type tag — skipping",
                        r.id
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "search_prompts: failed to load asset '{}': {e} — skipping",
                        r.id
                    );
                }
            }
        }
        Ok(prompts)
    }
    // ── Verification asset convenience methods (V33 阴轨验证契约) ────

    /// Save a [`VerificationAsset`] to the `verifications/` directory.
    ///
    /// Thin wrapper around [`save_asset`](Self::save_asset).
    pub async fn save_verification(
        &self,
        verification: &mut VerificationAsset,
    ) -> Result<(), TaijiError> {
        let mut asset = CognitiveAsset::Verification(verification.clone());
        verification.asset_type = "verification".into();
        self.save_asset(&mut asset).await?;
        verification.version = asset.version();
        Ok(())
    }

    /// Persist a Bayesian posterior asset（MVP-3.5 — BCP §6.4.1；version++ 原子写）。
    /// DMN Consumer 是唯一写者（TPN 执行期归藏只读 §8.3）。
    pub async fn save_model(
        &self,
        model: &mut ModelAsset,
    ) -> Result<(), TaijiError> {
        let mut asset = CognitiveAsset::Model(model.clone());
        model.header.asset_type = "model".into();
        self.save_asset(&mut asset).await?;
        model.header.version = asset.version();
        Ok(())
    }

    /// Load a single Bayesian posterior asset by id（None = 未初始化——调用方
    /// 从关联 verification 的 confidence 映射先验初始化）。
    pub async fn load_model(&self, id: &str) -> Result<Option<ModelAsset>, TaijiError> {
        match self.load_asset("model", id).await {
            Ok(CognitiveAsset::Model(m)) => Ok(Some(m)),
            Ok(_) => {
                tracing::warn!("asset '{id}' found in models/ but has wrong type tag");
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

    /// Load **all** Bayesian posterior assets（扫 models/*.yaml，跳过 .tmp）。
    /// 决策方（evolve_contracts / 主动学习）按 verification id 关联查找。
    pub async fn load_all_models(&self) -> Result<Vec<ModelAsset>, TaijiError> {
        let dir = self.data_dir.join("models");
        let mut models = Vec::new();
        if !dir.exists() {
            return Ok(models);
        }
        let mut read_dir = fs::read_dir(&dir).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to read models directory: {e}"),
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
                    let content = match fs::read_to_string(&path).await {
                        Ok(c) => c,
                        Err(err) => {
                            tracing::warn!("failed to read model {:?}: {err}", path);
                            continue;
                        }
                    };
                    match serde_yaml::from_str::<CognitiveAsset>(&content) {
                        Ok(CognitiveAsset::Model(m)) => models.push(m),
                        _ => {
                            tracing::warn!("model file not parseable: {:?}", path);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("error reading models directory entry: {e}");
                }
            }
        }
        Ok(models)
    }

    /// Load a [`VerificationAsset`] from the `verifications/` directory by id.
    ///
    /// Returns `None` when no asset with that id exists, so callers can
    /// gracefully fall back.
    pub async fn load_verification(
        &self,
        id: &str,
    ) -> Result<Option<VerificationAsset>, TaijiError> {
        match self.load_asset("verification", id).await {
            Ok(CognitiveAsset::Verification(v)) => Ok(Some(v)),
            Ok(_) => {
                tracing::warn!("asset '{id}' found in verifications/ but has wrong type tag");
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

    /// Load **all** verification contract assets from the `verifications/`
    /// directory.
    ///
    /// Direct directory scan (does **not** rely on `index.yaml` —
    /// `search_by_tags(&[])` returns empty, and the contract layer is small
    /// in MVP-1).  Individual file read/parse failures are surfaced as
    /// warnings and skipped (a corrupt contract asset must not block
    /// verification of other contracts); directory-level I/O failures are
    /// errors (无降级原则 — §8.20).
// ── Prompt 全量加载（V35/MVP-6：prompts 四算子对称演化需要）────

/// Load all prompt assets (active ones — pruned prompts are kept on disk for
/// audit but excluded from evolution/backprop, same semantics as verifications).
pub async fn load_all_prompts(&self) -> Result<Vec<PromptAsset>, TaijiError> {
    let dir = self.data_dir.join("prompts");
    let mut prompts = Vec::new();
    if !dir.exists() {
        return Ok(prompts);
    }
    let mut read_dir = fs::read_dir(&dir).await.map_err(|e| {
        TaijiError::KnowledgeStoreUnavailable {
            context: format!("failed to read prompts directory: {e}"),
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
                let content = match fs::read_to_string(&path).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to read prompt file — skipping"
                        );
                        continue;
                    }
                };
                match serde_yaml::from_str::<CognitiveAsset>(&content) {
                    Ok(CognitiveAsset::Prompt(p)) if p.status == "active" => prompts.push(p),
                    Ok(CognitiveAsset::Prompt(p)) => {
                        // V35/MVP-6：pruned 资产不参与演化/回传（保留文件供审计，
                        // 与 load_all_verifications 过滤语义一致）
                        tracing::debug!(
                            id = %p.id,
                            status = %p.status,
                            "skipping non-active prompt asset"
                        );
                    }
                    _ => {
                        tracing::warn!(
                            path = %path.display(),
                            "prompt asset has wrong type tag or is corrupt — skipping"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to read prompt directory entry — skipping"
                );
            }
        }
    }
    Ok(prompts)
}

    pub async fn load_all_verifications(
        &self,
    ) -> Result<Vec<VerificationAsset>, TaijiError> {
        let dir = self.data_dir.join("verifications");
        let mut verifications = Vec::new();
        if !dir.exists() {
            return Ok(verifications);
        }
        let mut read_dir = fs::read_dir(&dir).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to read verifications directory: {e}"),
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
                    match self.load_verification_from_path(&path).await {
                        Ok(Some(v)) if v.status == "active" => verifications.push(v),
                        Ok(Some(v)) => {
                            // MVP-3 演化：pruned 资产不参与加载/回传（保留文件供审计）
                            tracing::debug!(
                                id = %v.id,
                                status = %v.status,
                                "skipping non-active verification asset"
                            );
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::warn!("failed to load verification {:?}: {e}", path);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("error reading verifications directory entry: {e}");
                }
            }
        }
        Ok(verifications)
    }

    /// Load a single Verification asset from a specific file path.
    async fn load_verification_from_path(
        &self,
        path: &Path,
    ) -> Result<Option<VerificationAsset>, TaijiError> {
        let content = fs::read_to_string(path).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to read verification file {:?}: {e}", path),
            }
        })?;
        match serde_yaml::from_str::<CognitiveAsset>(&content) {
            Ok(CognitiveAsset::Verification(v)) => Ok(Some(v)),
            _ => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// CognitiveAsset enum (tagged union for serialisation)
// ---------------------------------------------------------------------------

/// A cognitive asset stored in the 理络 warehouse.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CognitiveAsset {
    #[serde(rename = "model")]
    Model(ModelAsset),
    #[serde(rename = "skill")]
    Skill(SkillAsset),
    #[serde(rename = "prompt")]
    Prompt(PromptAsset),
    #[serde(rename = "verification")]
    Verification(VerificationAsset),
}

impl CognitiveAsset {
    /// Return the asset type string (`"model"`, `"skill"`, `"prompt"`, `"verification"`).
    pub fn asset_type(&self) -> String {
        match self {
            CognitiveAsset::Model(_) => "model".into(),
            CognitiveAsset::Skill(_) => "skill".into(),
            CognitiveAsset::Prompt(_) => "prompt".into(),
            CognitiveAsset::Verification(_) => "verification".into(),
        }
    }

    /// Return the asset ID.
    pub fn id(&self) -> &str {
        match self {
            CognitiveAsset::Model(a) => &a.header.id,
            CognitiveAsset::Skill(a) => &a.header.id,
            CognitiveAsset::Prompt(a) => &a.id,
            CognitiveAsset::Verification(a) => &a.id,
        }
    }

    /// Return the asset version.
    pub fn version(&self) -> u32 {
        match self {
            CognitiveAsset::Model(a) => a.header.version,
            CognitiveAsset::Skill(a) => a.header.version,
            CognitiveAsset::Prompt(a) => a.version,
            CognitiveAsset::Verification(a) => a.version,
        }
    }

    /// Set the asset version.
    pub fn set_version(&mut self, v: u32) {
        match self {
            CognitiveAsset::Model(a) => a.header.version = v,
            CognitiveAsset::Skill(a) => a.header.version = v,
            CognitiveAsset::Prompt(a) => a.version = v,
            CognitiveAsset::Verification(a) => a.version = v,
        }
    }
}

/// V36：把未分区的旧根资产迁移到默认模型分区（BCP §6.1）——幂等。
///
/// 迁移对象：根目录的资产层（prompts/models/skills/verifications；
/// V38：不再迁移 truths/ 与 index.yaml——资产层已移除）。
/// 幂等规则：目标分区已存在同名目录/文件 → 跳过（可重复调用）；两者都不存在
/// → 跳过；仅源存在 → 移动。移动失败 → Err 上抛（带路径，诊断性——无降级原则
/// §23：迁移是数据完整性操作，不允许静默吞错）。
///
/// 调用时机：`build_engine`（所有命令入口）在 `LiluoClient::new` 之后调用一次。
pub async fn migrate_to_partitioned(
    root: &Path,
    default_key: &str,
) -> Result<(), TaijiError> {
    const ASSET_LAYERS: [&str; 4] = ["prompts", "models", "skills", "verifications"];
    let partition_dir = root.join(default_key);
    // rename 要求目标父目录存在——先建分区目录（幂等）。
    fs::create_dir_all(&partition_dir).await.map_err(|e| {
        TaijiError::KnowledgeStoreUnavailable {
            context: format!(
                "migrate_to_partitioned: failed to create partition dir {:?}: {e}",
                partition_dir
            ),
        }
    })?;

    for layer in ASSET_LAYERS {
        let src = root.join(layer);
        let dst = partition_dir.join(layer);
        let src_exists = fs::metadata(&src).await.is_ok();
        let dst_exists = fs::metadata(&dst).await.is_ok();
        if !src_exists || dst_exists {
            continue;
        }
        fs::rename(&src, &dst).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!(
                    "migrate_to_partitioned: failed to move {:?} → {:?}: {e}",
                    src, dst
                ),
            }
        })?;
        tracing::info!(
            layer,
            partition = %default_key,
            "migrated legacy asset layer into default partition"
        );
    }

    // V38：index.yaml 已移除——不再迁移（旧根目录遗留的 index.yaml 忽略，
    // 实时扫描不消费；如目标分区存在也不覆盖）。
    let src_index = root.join("index.yaml");
    let dst_index = partition_dir.join("index.yaml");
    let src_exists = fs::metadata(&src_index).await.is_ok();
    let dst_exists = fs::metadata(&dst_index).await.is_ok();
    if src_exists && !dst_exists {
        tracing::warn!(
            "legacy index.yaml found at root — V38 不再维护，跳过迁移（保留原文件）"
        );
    }

    Ok(())
}

/// V39 种子复制结果报告。
#[derive(Debug, Clone, Default)]
pub struct SeedReport {
    /// 实际复制到目标分区的资产数。
    pub copied: usize,
    /// 目标已存在而跳过的资产数（幂等）。
    pub skipped: usize,
    /// 源中 status=pruned 而不复制的资产数。
    pub pruned_skipped: usize,
}

/// 分区键合法性校验（V39）——`{provider}-{model}` slug 将拼接为目录路径，
/// 必须杜绝路径穿越与特殊字符（与 task_id 路径安全化同精神，AGENTS.md §19）。
/// 非法 → Err 上抛（无降级原则：CLI 输入即攻击面）。
fn validate_partition_key(key: &str) -> Result<(), TaijiError> {
    let invalid = key.is_empty()
        || key.contains(['/', '\\', '.', ' '])
        || key.contains("..")
        || key.chars().any(|c| c.is_control());
    if invalid {
        return Err(TaijiError::KnowledgeStoreUnavailable {
            context: format!(
                "invalid partition key '{key}': must be a {{provider}}-{{model}} slug \
                 (alphanumeric + '-') and must not contain path separators or '..'"
            ),
        });
    }
    Ok(())
}

/// V39 种子复制（BCP §6.1）——把源分区的活跃种子资产（`prompts/` +
/// `verifications/`，status != "pruned"）文件级复制到目标分区。
///
/// - 目标分区自动创建（`for_model` 语义：分区目录 + 四资产层）。
/// - **不复制** `models/`（贝叶斯后验 = 该模型的学习单元累积，新单元从零
///   开始——复制旧统计会污染路由 UCB）。
/// - version 保持原值（种子 = 内容快照，非演化写；目标不存在同名文件）。
/// - 幂等：目标已存在同名资产 → 跳过不覆盖。
/// - 源分区缺失 → Err 上抛（无降级原则）；单资产文件损坏 → warn 跳过。
///
/// # Errors
/// 分区键非法 / 源分区缺失 → `TaijiError::KnowledgeStoreUnavailable`。
///
/// 调用方：`taiji seed <target_key> [--from <source_key>]`（main.rs cmd_seed）。
pub async fn seed_partition(
    root: &Path,
    source_key: &str,
    target_key: &str,
) -> Result<SeedReport, TaijiError> {
    validate_partition_key(target_key)?;
    validate_partition_key(source_key)?;

    let source_dir = root.join(source_key);
    if !fs::metadata(&source_dir).await.map(|m| m.is_dir()).unwrap_or(false) {
        return Err(TaijiError::KnowledgeStoreUnavailable {
            context: format!(
                "seed_partition: source partition {:?} does not exist \
                 (run the source model's tasks first, or check the model key)",
                source_dir
            ),
        });
    }

    let partition = LiluoClient::for_model(&LiluoClient::new(root).await?, target_key).await?;
    let mut report = SeedReport::default();

    // 复制范围：prompts/ + verifications/（活跃种子资产）。
    for type_ in &["prompt", "verification"] {
        let layer = LiluoClient::type_dir_name(type_);
        let src_layer = source_dir.join(layer);
        if !fs::metadata(&src_layer).await.map(|m| m.is_dir()).unwrap_or(false) {
            continue;
        }
        let dst_layer = partition.data_dir.join(layer);
        fs::create_dir_all(&dst_layer).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("seed_partition: failed to create {:?}: {e}", dst_layer),
            }
        })?;

        let mut read_dir = fs::read_dir(&src_layer).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("seed_partition: failed to read {:?}: {e}", src_layer),
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
                    let Some(file_name) = path.file_name().map(|n| n.to_string_lossy().into_owned())
                    else {
                        continue;
                    };

                    // 解析验证（种子可读性）——单资产损坏仅 warn，不阻断其余。
                    let content = match fs::read_to_string(&path).await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(
                                "seed_partition: failed to read {:?}: {e} — skipping",
                                path
                            );
                            continue;
                        }
                    };
                    let Ok(asset) = serde_yaml::from_str::<CognitiveAsset>(&content) else {
                        tracing::warn!(
                            "seed_partition: unparseable asset {:?} — skipping",
                            path
                        );
                        continue;
                    };
                    // 过滤非活跃（pruned）资产。
                    if match &asset {
                        CognitiveAsset::Prompt(p) => p.status == "pruned",
                        CognitiveAsset::Verification(v) => v.status == "pruned",
                        _ => false,
                    } {
                        report.pruned_skipped += 1;
                        continue;
                    }

                    // 幂等：目标已存在 → 跳过不覆盖（种子不覆盖演化产物）。
                    let dst_path = dst_layer.join(&file_name);
                    if fs::metadata(&dst_path).await.is_ok() {
                        report.skipped += 1;
                        continue;
                    }

                    fs::copy(&path, &dst_path).await.map_err(|e| {
                        TaijiError::KnowledgeStoreUnavailable {
                            context: format!(
                                "seed_partition: failed to copy {:?} → {:?}: {e}",
                                path, dst_path
                            ),
                        }
                    })?;
                    report.copied += 1;
                    tracing::info!(
                        source = %source_key,
                        target = %target_key,
                        file = %file_name,
                        "seeded asset into target partition"
                    );
                }
                Err(e) => {
                    tracing::warn!("seed_partition: error reading directory entry: {e}");
                }
            }
        }
    }

    Ok(report)
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
        assert!(dir.join("models").exists());
        assert!(dir.join("skills").exists());
        assert!(dir.join("verifications").exists());
        // V38：不再创建 truths/ 与 index.yaml
        assert!(!dir.join("truths").exists());
        assert!(!dir.join("index.yaml").exists());
        cleanup(&dir).await;
    }

    // ── V36 分区（BCP §6.1）──────────────────────────────────────────

    #[tokio::test]
    async fn test_for_model_partitions_paths_and_isolates() {
        let dir = test_dir("for_model_partition").await;
        let root = LiluoClient::new(&dir).await.unwrap();

        // 根 client 写根资产
        let mut prompt = crate::types::agent::PromptAsset::new(
            "root-prompt",
            "根提示词",
            "root",
            "content",
            "FittingAgent",
            vec!["general".into()],
        );
        root.save_asset(&mut CognitiveAsset::Prompt(prompt))
            .await
            .unwrap();

        // 派生分区 client：活动目录 = root/{model_key}
        let partition = root.for_model("deepseek-deepseek-chat").await.unwrap();
        assert_eq!(partition.partition_key(), Some("deepseek-deepseek-chat"));
        assert!(dir.join("deepseek-deepseek-chat").exists());
        assert!(dir.join("deepseek-deepseek-chat/prompts").exists());
        assert!(dir.join("deepseek-deepseek-chat/verifications").exists());
        // V38：分区不再创建 index.yaml
        assert!(!dir.join("deepseek-deepseek-chat/index.yaml").exists());
        // root_dir 恒为 knowledge 根（model_stats 层）
        assert_eq!(partition.root_dir(), &dir);

        // 分区内写资产，根不可见（隔离）
        let mut p = crate::types::agent::PromptAsset::new(
            "partition-prompt",
            "分区提示词",
            "p",
            "content",
            "FittingAgent",
            vec!["general".into()],
        );
        partition
            .save_asset(&mut CognitiveAsset::Prompt(p))
            .await
            .unwrap();
        let root_loaded = root.load_asset("prompt", "partition-prompt").await;
        assert!(root_loaded.is_err(), "partition asset must not leak to root");
        let part_loaded = partition.load_asset("prompt", "partition-prompt").await;
        assert!(part_loaded.is_ok(), "partition asset loadable in partition");

        // 搜索也按分区隔离
        let root_hits = root.search_prompts(&["general"]).await.unwrap();
        let part_hits = partition.search_prompts(&["general"]).await.unwrap();
        assert!(
            !root_hits.iter().any(|x| x.id == "partition-prompt"),
            "root search must not see partition assets"
        );
        assert!(part_hits.iter().any(|x| x.id == "partition-prompt"));

        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_migrate_to_partitioned_idempotent() {
        let dir = test_dir("migrate_partition").await;
        let root = LiluoClient::new(&dir).await.unwrap();

        // 根资产层放一个资产
        let mut prompt = crate::types::agent::PromptAsset::new(
            "legacy-prompt",
            "旧根提示词",
            "legacy",
            "content",
            "FittingAgent",
            vec!["legacy".into()],
        );
        root.save_asset(&mut CognitiveAsset::Prompt(prompt))
            .await
            .unwrap();
        assert!(dir.join("prompts/legacy-prompt.yaml").exists());

        // 首次迁移：prompts/ 整体移入默认分区
        migrate_to_partitioned(&dir, "deepseek-deepseek-chat")
            .await
            .unwrap();
        assert!(!dir.join("prompts/legacy-prompt.yaml").exists());
        assert!(dir
            .join("deepseek-deepseek-chat/prompts/legacy-prompt.yaml")
            .exists());

        // 幂等：重复调用无操作不报错
        migrate_to_partitioned(&dir, "deepseek-deepseek-chat")
            .await
            .unwrap();

        // 分区 client 可读迁移后的资产
        let partition = root.for_model("deepseek-deepseek-chat").await.unwrap();
        let loaded = partition.load_asset("prompt", "legacy-prompt").await;
        assert!(loaded.is_ok(), "migrated asset readable in partition");

        cleanup(&dir).await;
    }

    // ── V39 种子复制（taiji seed）────────────────────────────────────

    #[tokio::test]
    async fn test_seed_partition_copies_active_seeds_and_skips_pruned() {
        let dir = test_dir("seed_copy").await;
        let root = LiluoClient::new(&dir).await.unwrap();

        // 源分区：一个 active prompt + 一个 pruned prompt + 一个 active verification
        let src = root.for_model("deepseek-deepseek-src").await.unwrap();
        let mut p = crate::types::agent::PromptAsset::new(
            "seed-prompt",
            "种子提示词",
            "seed",
            "content",
            "FittingAgent",
            vec!["general".into()],
        );
        src.save_asset(&mut CognitiveAsset::Prompt(p)).await.unwrap();

        let mut pruned = crate::types::agent::PromptAsset::new(
            "pruned-prompt",
            "淘汰提示词",
            "p",
            "content",
            "FittingAgent",
            vec!["general".into()],
        );
        pruned.status = "pruned".into();
        src.save_asset(&mut CognitiveAsset::Prompt(pruned)).await.unwrap();

        let mut v = crate::types::agent::VerificationAsset::new(
            "seed-verification",
            "种子契约",
            "seed",
            "content",
            Vec::new(),
            vec!["general".into()],
        );
        src.save_asset(&mut CognitiveAsset::Verification(v)).await.unwrap();

        // 目标分区：已存在一个同名资产（幂等跳过测试用）
        let dst = root.for_model("deepseek-deepseek-dst").await.unwrap();
        let mut existing = crate::types::agent::PromptAsset::new(
            "seed-prompt",
            "已存在",
            "x",
            "content",
            "FittingAgent",
            vec!["general".into()],
        );
        dst.save_asset(&mut CognitiveAsset::Prompt(existing))
            .await
            .unwrap();

        let report = seed_partition(&dir, "deepseek-deepseek-src", "deepseek-deepseek-dst")
            .await
            .unwrap();
        // 复制：seed-verification（seed-prompt 被目标已存在跳过，pruned 排除）
        assert_eq!(report.copied, 1);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.pruned_skipped, 1);

        // 幂等：二次调用全部跳过
        let report2 = seed_partition(&dir, "deepseek-deepseek-src", "deepseek-deepseek-dst")
            .await
            .unwrap();
        assert_eq!(report2.copied, 0);
        assert!(report2.skipped >= 2);

        // pruned 未复制；models/ 不复制
        let dst_loaded = dst.load_asset("prompt", "pruned-prompt").await;
        assert!(dst_loaded.is_err(), "pruned asset must not be seeded");
        assert!(!dir.join("deepseek-deepseek-dst/models").join("seed-verification.yaml").exists());

        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_seed_partition_missing_source_errors() {
        let dir = test_dir("seed_missing_source").await;
        LiluoClient::new(&dir).await.unwrap();

        let err = seed_partition(&dir, "deepseek-no-such-model", "deepseek-deepseek-dst")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("source partition"), "{err}");

        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_seed_partition_invalid_key_errors() {
        let dir = test_dir("seed_invalid_key").await;
        LiluoClient::new(&dir).await.unwrap();

        // 路径穿越 / 非法字符一律拒绝（CLI 输入即攻击面）。
        for bad in ["../evil", "a/b", "a\\b", "a b", "a.b", ""] {
            let err = seed_partition(&dir, "deepseek-deepseek-src", bad)
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("invalid partition key"),
                "key '{bad}' must be rejected: {err}"
            );
        }

        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_model_stats_roundtrip() {
        let dir = test_dir("model_stats").await;
        let root = LiluoClient::new(&dir).await.unwrap();

        // 缺失 → 空表（合法状态）
        let stats = root.load_model_stats().await.unwrap();
        assert!(stats.is_empty());

        // 写回 + 读回
        let mut stats = stats;
        stats.insert(
            "deepseek-deepseek-chat".to_string(),
            crate::types::agent::ModelStatsRow {
                n: 3,
                pass_count: 2,
                cost_sum: 1000,
                quality_sum: 2.5,
                rounds_sum: 4,
            },
        );
        root.save_model_stats(&stats).await.unwrap();
        let loaded = root.load_model_stats().await.unwrap();
        let row = loaded.get("deepseek-deepseek-chat").unwrap();
        assert_eq!(row.n, 3);
        assert_eq!(row.pass_count, 2);
        assert_eq!(row.cost_sum, 1000);
        assert!((row.quality_sum - 2.5).abs() < 1e-9);
        assert_eq!(row.rounds_sum, 4);

        // 分区 client 的 root_dir 恒为根——model_stats 跨分区可见
        let partition = root.for_model("deepseek-other").await.unwrap();
        let p_stats = partition.load_model_stats().await.unwrap();
        assert_eq!(p_stats.get("deepseek-deepseek-chat").unwrap().n, 3);

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
    async fn test_save_and_load_skill() {
        let dir = test_dir("save_load_skill").await;
        let client = LiluoClient::new(&dir).await.unwrap();

        let mut asset = CognitiveAsset::Skill(SkillAsset {
            header: AssetHeader {
                asset_type: "skill".into(),
                layer: 1,
                id: "skill-001".into(),
                name: "Test Skill".into(),
                description: "A skill for testing".into(),
                tags: vec!["test".into(), "demo".into()],
                confidence: 0.95,
                version: 0, // will be set to 1 on save
            },
            tool_name: "calc".into(),
            trigger_pattern: "calc".into(),
            task_type_tags: vec!["math".into()],
            success_count: 0,
            fail_count: 0,
        });

        client.save_asset(&mut asset).await.unwrap();
        assert_eq!(asset.version(), 1);

        // Load back
        let loaded = client.load_asset("skill", "skill-001").await.unwrap();
        match loaded {
            CognitiveAsset::Skill(s) => {
                assert_eq!(s.header.id, "skill-001");
                assert_eq!(s.header.version, 1);
                assert!(s.header.tags.contains(&"test".to_string()));
            }
            _ => panic!("expected Skill asset"),
        }

        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_save_increments_version() {
        let dir = test_dir("save_increments_version").await;
        let client = LiluoClient::new(&dir).await.unwrap();

        let mut asset = CognitiveAsset::Skill(SkillAsset {
            header: AssetHeader {
                asset_type: "skill".into(),
                layer: 1,
                id: "skill-ver".into(),
                name: "Version Test".into(),
                description: "Testing version++".into(),
                tags: vec!["test".into()],
                confidence: 0.9,
                version: 0,
            },
            tool_name: "calc".into(),
            trigger_pattern: "calc".into(),
            task_type_tags: vec!["math".into()],
            success_count: 0,
            fail_count: 0,
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

        // Save a prompt with tag "math".
        let mut asset = CognitiveAsset::Prompt(crate::types::agent::PromptAsset::new(
            "prompt-math",
            "Math Prompt",
            "",
            "content",
            "FittingAgent",
            vec!["math".into(), "logic".into()],
        ));
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

        // Search by "logic" — should get only the prompt.
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
    async fn test_load_nonexistent_asset_returns_error() {
        let dir = test_dir("load_nonexistent").await;
        let client = LiluoClient::new(&dir).await.unwrap();

        let result = client.load_asset("prompt", "nonexistent").await;
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
            vec!["fitting".into(), "execution".into()],
        );
        client.save_prompt(&mut p1).await.unwrap();

        let mut p2 = crate::types::agent::PromptAsset::new(
            "exec-verify",
            "执行验证提示词",
            "Execution mode CausalAgent verify prompt",
            "你是因果验证器（执行模式）...",
            "CausalAgent",
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

    /// V32 回归测试：手写/外部生成的资产 YAML 可能省略可选字段
    /// （agent_target / usage_count / success_rate）——缺失时必须能反序列化
    /// 并加载，不得导致整条资产加载失败（系统性 bug 实测：资产缺字段 →
    /// search_prompts 吞错 → MetaAgent 零资产降级 → 编排失效）。
    #[tokio::test]
    async fn test_load_prompt_with_missing_optional_fields() {
        let dir = test_dir("prompt_missing_fields").await;
        let client = LiluoClient::new(&dir).await.unwrap();

        // 手写 YAML：只含核心字段，省略 agent_target/usage_count/success_rate。
        let yaml = r#"
type: prompt
id: hand-written-prompt
layer: 1
name: 手写资产
confidence: 0.5
version: 1
tags:
  - general
content: 手写内容
"#;
        let path = dir.join("prompts").join("hand-written-prompt.yaml");
        fs::write(&path, yaml).await.unwrap();

        // load_prompt 必须成功，缺失字段取默认值。
        let loaded = client.load_prompt("hand-written-prompt").await.unwrap();
        let p = loaded.expect("hand-written prompt should load with defaults");
        assert_eq!(p.id, "hand-written-prompt");
        assert_eq!(p.agent_target, "");
        assert_eq!(p.usage_count, 0);
        assert_eq!(p.success_rate, 0.0);

        // 手写文件绕过 save_asset——V38 实时扫描直接命中（无索引缓存）。
        // search_prompts 必须能命中。
        let results = client.search_prompts(&["general"]).await.unwrap();
        assert!(
            results.iter().any(|x| x.id == "hand-written-prompt"),
            "hand-written prompt should be searchable"
        );

        cleanup(&dir).await;
    }
}

#[cfg(test)]
mod model_asset_tests {
    use super::*;

    /// V33/MVP-3.5：先验映射（§6.4.1）——confidence → α/β，边界 clamp。
    #[test]
    fn model_asset_from_prior_maps_confidence() {
        // confidence 0.8, k=10 → α=9, β=3 → μ=0.75
        let m = ModelAsset::from_prior("v1", "v1", 0.8, 10.0);
        assert!((m.alpha - 9.0).abs() < 1e-9);
        assert!((m.beta - 3.0).abs() < 1e-9);
        assert!((m.posterior_mean() - 0.75).abs() < 1e-9);
        // confidence 0.5 → 中性先验 μ=0.5
        let m = ModelAsset::from_prior("v2", "v2", 0.5, 10.0);
        assert!((m.posterior_mean() - 0.5).abs() < 1e-9);
        // 越界 confidence clamp
        let m = ModelAsset::from_prior("v3", "v3", 1.5, 10.0);
        assert!((m.posterior_mean() - (1.0 + 10.0) / (2.0 + 10.0)).abs() < 1e-9);
        // k=0 → Beta(1,1) 均匀先验 μ=0.5
        let m = ModelAsset::from_prior("v4", "v4", 0.9, 0.0);
        assert!((m.alpha - 1.0).abs() < 1e-9);
        assert!((m.beta - 1.0).abs() < 1e-9);
    }

    /// V33/MVP-3.5：后验标准差随采样收缩（σ = √(αβ/((α+β)²·(α+β+1)))）。
    #[test]
    fn model_asset_posterior_sigma_shrinks_with_samples() {
        // 纯先验（k=10, c=0.5）：α=β=6 → σ ≈ 0.1387
        let prior = ModelAsset::from_prior("p", "p", 0.5, 10.0);
        let sigma_prior = prior.posterior_sigma();
        // 大量采样（+100 成功）：σ 显著收缩
        let mut sampled = prior.clone();
        sampled.alpha += 100.0;
        let sigma_sampled = sampled.posterior_sigma();
        assert!(
            sigma_sampled < sigma_prior * 0.5,
            "sigma shrinks with data: {sigma_prior} → {sigma_sampled}"
        );
        // 无数据退化（α+β ≤ 0）→ 0.0
        let empty = ModelAsset::from_prior("e", "e", 0.5, 0.0);
        let mut neg = empty.clone();
        neg.alpha = -1.0;
        neg.beta = -1.0;
        assert_eq!(neg.posterior_sigma(), 0.0);
        assert_eq!(neg.posterior_mean(), 0.0);
    }
}

// ── UCB 检索排序（V35/MVP-5：检索数学化，BCP §6.3 实现层定稿）────

/// UCB 检索排序（纯函数，确定性）——prompts 检索从「手填 confidence 降序」
/// 升级为「贝叶斯后验均值 + UCB 探索项」（§6.3 实现层定稿）：
///
/// ```text
/// score(id) = μ(id) + C · √( ln N_total / (n_id + 1) )
/// μ(id) = ModelAsset 后验均值（存在）；否则 §6.4.1 先验映射
///         α = 1 + k·confidence, β = 1 + k·(1−confidence) → μ = α/(α+β)
/// n_id  = stats.n（任务级采样，MVP-6 起回传写入）
/// ```
///
/// 确定性保证：n+1 平滑（n=0 时有限探索分，冷启动退化为先验 μ 降序）；
/// score 相等时按 id 字典序（与 read_dir 顺序无关）。返回降序索引。
pub(crate) fn rank_prompts_by_ucb(
    prompts: &[PromptAsset],
    models: &[ModelAsset],
    c: f64,
    prior_strength: f64,
) -> Vec<usize> {
    let total_n: f64 = prompts.iter().map(|p| p.stats.n as f64).sum();
    let n_total = total_n.max(1.0);

    let mut scores: Vec<(f64, &str, usize)> = prompts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mu = match models.iter().find(|m| m.header.id == p.id) {
                Some(m) => m.posterior_mean(),
                None => {
                    let c = p.confidence.clamp(0.0, 1.0);
                    let alpha = 1.0 + prior_strength.max(0.0) * c;
                    let beta = 1.0 + prior_strength.max(0.0) * (1.0 - c);
                    alpha / (alpha + beta)
                }
            };
            let n_node = p.stats.n as f64;
            let explore = c * (n_total.ln() / (n_node + 1.0)).sqrt();
            (mu + explore, p.id.as_str(), i)
        })
        .collect();

    // 降序；score 相等 → id 字典序（确定性二级键）
    scores.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(b.1))
    });
    scores.into_iter().map(|(_, _, i)| i).collect()
}



#[cfg(test)]
mod ucb_rank_tests {
    use super::*;
    use crate::types::verification::CheckStats;

    fn mk_p(id: &str, confidence: f64, n: u64, pass: u64) -> PromptAsset {
        let mut p = PromptAsset::new(id, id, "t", "t", "FittingAgent", vec!["x".into()]);
        p.confidence = confidence;
        p.stats = CheckStats { n, pass_count: pass, ..Default::default() };
        p
    }

    /// V35/MVP-5：冷启动（全 n=0）→ 退化为先验 μ 降序（确定性——§6.3 实现层定稿）。
    #[test]
    fn ucb_rank_cold_start_falls_back_to_prior_mu() {
        let prompts = vec![
            mk_p("p-low", 0.5, 0, 0),
            mk_p("p-high", 0.95, 0, 0),
            mk_p("p-mid", 0.8, 0, 0),
        ];
        let ranked = rank_prompts_by_ucb(&prompts, &[], 1.414, 10.0);
        // 先验 μ：0.95→(1+9.5)/(2+10)=0.9583；0.8→0.9；0.5→0.5
        let ids: Vec<&str> = ranked.iter().map(|&i| prompts[i].id.as_str()).collect();
        assert_eq!(ids, vec!["p-high", "p-mid", "p-low"], "cold start = prior μ desc");
    }

    /// V35/MVP-5：n>0 时探索分生效——高先验但已充分采样 vs 低采样同先验，
    /// 低采样者获得探索加分（(n+1) 平滑）。
    #[test]
    fn ucb_rank_exploration_bonus_for_low_sample() {
        let prompts = vec![
            mk_p("p-sampled", 0.9, 50, 45),
            mk_p("p-fresh", 0.9, 0, 0),
        ];
        let ranked = rank_prompts_by_ucb(&prompts, &[], 1.414, 10.0);
        assert_eq!(prompts[ranked[0]].id, "p-fresh", "n=0 gets exploration bonus");
    }

    /// V35/MVP-5：确定性——同输入两次排序结果一致（数学排序，可复现）。
    #[test]
    fn ucb_rank_deterministic() {
        let prompts = vec![
            mk_p("a", 0.6, 3, 2),
            mk_p("b", 0.6, 3, 2),
            mk_p("c", 0.6, 3, 2),
        ];
        let r1 = rank_prompts_by_ucb(&prompts, &[], 1.414, 10.0);
        let r2 = rank_prompts_by_ucb(&prompts, &[], 1.414, 10.0);
        assert_eq!(r1, r2, "same input → same ranking (id lexicographic tiebreak)");
    }
}
