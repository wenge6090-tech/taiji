//! GuizangClient — 归藏 (cognitive warehouse) file-system client.
//!
//! Cognitive assets (Prompts, Models, Skills, Verifications) are stored as
//! YAML files under `{data_dir}/{type}s/{id}.yaml`. V38: no `index.yaml` —
//! tag search scans directories on demand (`scan_assets`).
//!
//! # Directory layout (V43：yang/yin Skills 对偶子树，BCP §10.1)
//!
//! ```text
//! {data_dir}/
//! ├── yang/
//! │   ├── prompts/            # 阳系统提示词
//! │   └── skills/
//! │       ├── orch/            # 编排 Skill
//! │       └── exec/            # 执行 Skill
//! ├── yin/
//! │   ├── prompts/            # 阴系统提示词
//! │   └── skills/
//! │       ├── verify/          # 验证 Skill（原 verifications/）
//! │       └── converge/        # 收敛 Skill
//! ├── models/                 # L2 Model assets（贝叶斯后验）
//! └── skills/                 # L1 技能统计元数据（旧兼容）
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

/// 旧 L1 工具注册资产（CognitiveAsset::Skill 历史形态）。
///
/// V45：与 [`crate::types::verification::SkillAsset`]（统一 Skill 双轨）**同名冲突已消除**——
/// 本类型仅保留 serde/测试兼容；新代码一律用 `types::verification::SkillAsset` +
/// [`GuizangClient::save_skill`]/[`GuizangClient::load_skill_assets`]。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyToolSkillAsset {
    #[serde(flatten)]
    pub header: AssetHeader,
    pub tool_name: String,
    pub trigger_pattern: String,
    pub task_type_tags: Vec<String>,
    pub success_count: u64,
    pub fail_count: u64,
}

/// 兼容别名——过渡期保留；新代码禁止使用。
#[deprecated(note = "use types::verification::SkillAsset + GuizangClient::save_skill")]
pub type SkillAsset = LegacyToolSkillAsset;

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
// GuizangClient
// ---------------------------------------------------------------------------

/// File-system-based 归藏 (cognitive warehouse) client.
///
/// # Thread safety
/// `GuizangClient` is `Send + Sync`.  Internal state (`data_dir`) is immutable
/// after construction.
///
/// # V44 去分区化（BCP §10.1）
/// 单一资产树：`data_dir` = knowledge 根，直接承载 yang/ + yin/ + models/。
/// 模型维度仅在统计层区分（`model_stats.yaml` 按 model_key 索引），
/// 不产生资产副本。V36-V43 的 `for_model` 分区派生已删除。
#[derive(Debug)]
pub struct GuizangClient {
    /// knowledge 根目录（构造时传入）——资产树 + model_stats.yaml 所在层。
    data_dir: PathBuf,
}

/// Compatibility alias — 旧代码中的 `LiluoClient` 等效于 `GuizangClient`。
pub type LiluoClient = GuizangClient;

impl GuizangClient {
    /// Directory name for each asset type within `data_dir`.
    fn type_dir_name(type_: &str) -> &'static str {
        match type_ {
            "model" => "models",
            "skill" => "skills",
            "prompt" => "prompts",
            "verification" => "verifications",
            // BCP §10.1 yang/yin 对偶目录（V42 迁移）:
            "yang_prompt" => "yang/prompts",
            "yin_prompt" => "yin/prompts",
            "yin_verification" => "yin/skills/verify",
            // V43: yin/skills/ 嵌套类别
            "yin_skill_verify" => "yin/skills/verify",
            "yin_skill_converge" => "yin/skills/converge",
            _ => {
                tracing::warn!("unknown cognitive asset type: {type_}, defaulting to 'prompts'");
                "prompts"
            }
        }
    }

    // ── Constructors ──────────────────────────────────────────────────

    /// Create a new `GuizangClient`, ensuring the root directory exists.
    ///
    /// V44：创建根级资产层目录（yang/ + yin/ + models/，BCP §10.1）。
    ///
    /// # Errors
    /// Returns `TaijiError::IO` if the root directory cannot be created.
    pub async fn new(data_dir: &Path) -> Result<Self, TaijiError> {
        fs::create_dir_all(data_dir).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to create knowledge root {:?}: {e}", data_dir),
            }
        })?;
        let this = Self {
            data_dir: data_dir.to_path_buf(),
        };
        this.ensure_dirs().await?;
        Ok(this)
    }

    /// Create a sparse `GuizangClient` that skips directory creation.
    ///
    /// V44：与 [`new`](Self::new) 同语义（根级资产树）；不建目录，
    /// 适合只读/迁移场景。
    pub async fn new_sparse(data_dir: &Path) -> Result<Self, TaijiError> {
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
        })
    }

    /// Create directories for all asset types (yang/yin 对偶，BCP §10.1，V44 根级)。
    async fn ensure_dirs(&self) -> Result<(), TaijiError> {
        let dirs = [
            self.data_dir.join("models"),
            self.data_dir.join("yang/prompts"),           // 阳轨 FittingAgent 提示词
            self.data_dir.join("yang/skills/orch"),       // 阳轨编排 Skill
            self.data_dir.join("yang/skills/exec"),       // 阳轨执行 Skill
            self.data_dir.join("yin/prompts"),            // 阴轨 CausalAgent 提示词
            self.data_dir.join("yin/skills/verify"),      // 阴轨验证 Skill（BCP §10.1）
            self.data_dir.join("yin/skills/converge"),    // 阴轨收敛 Skill（BCP §10.1）
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
        let path = self.data_dir.join("model_stats.yaml");
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
        let path = self.data_dir.join("model_stats.yaml");
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

        for type_ in &["model", "skill", "prompt", "verification", "yang_prompt", "yin_prompt", "yin_skill_verify", "yin_skill_converge"] {
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

    /// Save a [`PromptAsset`] to the appropriate directory:
    /// - `agent_target="FittingAgent"` → `yang/prompts/`（BCP §10.1 阳轨）
    /// - `agent_target="CausalAgent"` → `yin/prompts/`（BCP §10.1 阴轨）
    /// - 空或其他 → `prompts/`（旧兼容）
    pub async fn save_prompt(&self, prompt: &mut PromptAsset) -> Result<(), TaijiError> {
        let type_str = match prompt.agent_target.as_str() {
            "FittingAgent" => "yang_prompt",
            "CausalAgent" => "yin_prompt",
            _ => "prompt",
        };
        prompt.asset_type = type_str.into();
        let mut asset = CognitiveAsset::Prompt(prompt.clone());
        self.save_asset(&mut asset).await?;
        prompt.version = asset.version();
        Ok(())
    }

    /// Load a [`PromptAsset`] from the 理络 `prompts/` directory by name.
    ///
    /// Returns `None` when no asset with that name exists (as opposed to
    /// returning an error), so callers can gracefully fall back.
    pub async fn load_prompt(&self, name: &str) -> Result<Option<PromptAsset>, TaijiError> {
        // V43：按 yang/prompts/ → yin/prompts/ → prompts/ 顺序尝试
        for type_ in ["yang_prompt", "yin_prompt", "prompt"] {
            match self.load_asset(type_, name).await {
                Ok(CognitiveAsset::Prompt(p)) => return Ok(Some(p)),
                Ok(_) => {
                    tracing::warn!("asset '{name}' found but has wrong type tag");
                    return Ok(None);
                }
                Err(e) => {
                    if e.to_string().contains("failed to read asset") {
                        continue; // 尝试下一个路径
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        Ok(None)
    }

    /// Search for prompt assets by task-type tags.
    ///
    /// Calls the generic [`search_by_tags`](Self::search_by_tags) then loads
    /// only assets whose type is `"prompt"`.
    pub async fn search_prompts(&self, tags: &[&str]) -> Result<Vec<PromptAsset>, TaijiError> {
        let refs = self.search_by_tags(tags).await?;
        let mut prompts = Vec::new();
        for r in &refs {
            if r.asset_type != "prompt" && r.asset_type != "yang_prompt" && r.asset_type != "yin_prompt" {
                continue;
            }
            let load_type = if r.asset_type == "yang_prompt" || r.asset_type == "yin_prompt" {
                r.asset_type.as_str()
            } else {
                "prompt"
            };
            match self.load_asset(load_type, &r.id).await {
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

    /// Save a [`VerificationAsset`] to the `yin/skills/verify/` directory（BCP §10.1 阴轨）。
    ///
    /// Thin wrapper around [`save_asset`](Self::save_asset).
    pub async fn save_verification(
        &self,
        verification: &mut VerificationAsset,
    ) -> Result<(), TaijiError> {
        verification.asset_type = "yin_skill_verify".into();
        let mut asset = CognitiveAsset::Verification(verification.clone());
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
        model.header.asset_type = "model".into();
        let mut asset = CognitiveAsset::Model(model.clone());
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

    /// Load a [`VerificationAsset`] from the `yin/skills/verify/` directory by id.
    ///
    /// Returns `None` when no asset with that id exists, so callers can
    /// gracefully fall back.
    pub async fn load_verification(
        &self,
        id: &str,
    ) -> Result<Option<VerificationAsset>, TaijiError> {
        match self.load_asset("yin_skill_verify", id).await {
            Ok(CognitiveAsset::Verification(v)) => Ok(Some(v)),
            Ok(_) => {
                tracing::warn!("asset '{id}' found in skills/verify/ but has wrong type tag");
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

    /// Load **all** verification contract assets from the `yin/skills/verify/`
    /// directory（BCP §10.1）。
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
/// V43：扫描 yang/prompts/ + yin/prompts/ + prompts/（旧兼容）。
pub async fn load_all_prompts(&self) -> Result<Vec<PromptAsset>, TaijiError> {
    let mut prompts = Vec::new();
    for dir_name in ["yang/prompts", "yin/prompts", "prompts"] {
        let dir = self.data_dir.join(dir_name);
        if !dir.exists() {
            continue;
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
                        Ok(CognitiveAsset::Prompt(p)) if p.status == "active" => {
                            // 去重：同一 id 只保留首次（yang/prompts 优先）
                            if !prompts.iter().any(|existing: &PromptAsset| existing.id == p.id) {
                                prompts.push(p);
                            }
                        }
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
    }
    Ok(prompts)
}

    /// 加载全部 active 验证契约（V45 双轨兼容）。
    ///
    /// 扫描顺序（同 id 首次优先）：
    /// 1. 新文件夹 `yin/skills/verify/{id}/skill.yaml`（SkillAsset → VerificationAsset）
    /// 2. 旧扁平 `yin/skills/verify/*.yaml`（**原样** VerificationAsset——保留 checks.stats / variant_of，
    ///    DMN evolver 依赖这些字段，禁止经 SkillAsset 往返丢字段）
    /// 3. 元层 verify 判据——**仅磁盘为空时**注入（冷启动保底；有磁盘资产时不混入，
    ///    避免污染 evolver 计数。运行时 verify 仍走 catalog 元层∪资产层）
    pub async fn load_all_verifications(
        &self,
    ) -> Result<Vec<VerificationAsset>, TaijiError> {
        let mut verifications = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let dir = self.data_dir.join("yin/skills/verify");

        if dir.exists() {
            let mut read_dir = fs::read_dir(&dir).await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to read {:?} directory: {e}", dir),
                }
            })?;
            let mut subdirs: Vec<PathBuf> = Vec::new();
            let mut legacy_files: Vec<PathBuf> = Vec::new();
            while let Some(entry) = read_dir.next_entry().await.transpose() {
                let e = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!("error reading {:?} entry: {e}", dir);
                        continue;
                    }
                };
                let path = e.path();
                if path.is_dir() {
                    subdirs.push(path);
                } else if path.extension().is_some_and(|x| x == "yaml")
                    && path
                        .file_name()
                        .is_none_or(|n| !n.to_string_lossy().ends_with(".tmp"))
                {
                    legacy_files.push(path);
                }
            }

            // 1. 文件夹 skill.yaml 优先
            for subdir in &subdirs {
                let sf = subdir.join("skill.yaml");
                if !sf.exists() {
                    continue;
                }
                match self.load_verification_from_path(&sf).await {
                    Ok(Some(v)) if v.status == "active" && !seen.contains(&v.id) => {
                        seen.insert(v.id.clone());
                        verifications.push(v);
                    }
                    Ok(Some(v)) => {
                        tracing::debug!(id = %v.id, status = %v.status, "skip non-active/dup skill.yaml");
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("failed to load skill.yaml {:?}: {e}", sf),
                }
            }

            // 2. 旧扁平 VerificationAsset（原样保留 stats/variant_of）
            for path in &legacy_files {
                match self.load_verification_from_path(path).await {
                    Ok(Some(v)) if v.status == "active" && !seen.contains(&v.id) => {
                        seen.insert(v.id.clone());
                        verifications.push(v);
                    }
                    Ok(Some(v)) => {
                        tracing::debug!(id = %v.id, status = %v.status, "skip non-active/dup legacy");
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("failed to load verification {:?}: {e}", path),
                }
            }
        }

        // 3. 元层保底（仅空库）
        if verifications.is_empty() {
            for s in crate::infra::meta_skills::meta_skills(
                crate::types::verification::SkillCategory::Verify,
            ) {
                verifications.push(Self::skill_asset_to_verification(&s));
            }
        }

        Ok(verifications)
    }

    /// SkillAsset → VerificationAsset（DMN/evolver 过渡桥——保留 checks 形态）。
    fn skill_asset_to_verification(
        s: &crate::types::verification::SkillAsset,
    ) -> VerificationAsset {
        use crate::types::verification::{CheckKind, CheckSpec, CheckStats, SkillKind};
        let checks: Vec<CheckSpec> = s
            .implementations
            .iter()
            .enumerate()
            .filter_map(|(idx, impl_)| {
                let kind = match impl_.kind {
                    SkillKind::FileExists => CheckKind::FileExists,
                    SkillKind::SchemaValid => CheckKind::SchemaValid,
                    SkillKind::ReferenceResolves => CheckKind::ReferenceResolves,
                    SkillKind::CommandSucceeds => CheckKind::CommandSucceeds,
                    SkillKind::LlmJudgement => CheckKind::LlmJudgement,
                    SkillKind::TraceConsistency => CheckKind::TraceConsistency,
                    // 阳面 kind 不进 VerificationAsset.checks
                    _ => return None,
                };
                // 空 command 的 CommandSucceeds 不落盘给 DMN（避免 soft-fail 噪声）
                if kind == CheckKind::CommandSucceeds {
                    let cmd = impl_
                        .params
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim();
                    if cmd.is_empty() {
                        return None;
                    }
                }
                Some(CheckSpec {
                    id: format!("{}#{idx}", s.id),
                    kind,
                    target: impl_.target.clone(),
                    params: impl_.params.clone(),
                    severity: impl_.severity.clone().unwrap_or_default(),
                    pass_condition: impl_.pass_condition.clone(),
                    stats: CheckStats::default(),
                })
            })
            .collect();
        VerificationAsset {
            asset_type: "yin_skill_verify".into(),
            layer: 0,
            id: s.id.clone(),
            name: s.name.clone(),
            description: s.description.clone(),
            tags: s.tags.clone(),
            confidence: s.confidence,
            version: s.version.max(1),
            content: s.description.clone(),
            checks,
            agent_target: s.agent_target.clone(),
            usage_count: 0,
            success_rate: 0.0,
            status: s.status.clone(),
            variant_of: s.variant_of.clone(),
        }
    }

    /// Load a single Verification asset from a specific file path.
    ///
    /// V45：同时识别旧 `type: verification` 与新 `type: skill`（文件夹 skill.yaml）。
    async fn load_verification_from_path(
        &self,
        path: &Path,
    ) -> Result<Option<VerificationAsset>, TaijiError> {
        let content = fs::read_to_string(path).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to read verification file {:?}: {e}", path),
            }
        })?;
        // 旧 VerificationAsset 路径
        if let Ok(CognitiveAsset::Verification(v)) = serde_yaml::from_str::<CognitiveAsset>(&content)
        {
            return Ok(Some(v));
        }
        // V45 SkillAsset 路径（skill.yaml）
        if let Ok(s) =
            serde_yaml::from_str::<crate::types::verification::SkillAsset>(&content)
        {
            return Ok(Some(Self::skill_asset_to_verification(&s)));
        }
        Ok(None)
    }

    /// V43: 按 SkillCategory 加载全部 active Skill 资产（BCP §10.1）。
    ///
    /// - `Verify` → 扫描 `yin/skills/verify/`
    /// - `Converge` → 扫描 `yin/skills/converge/`
    ///
    /// 去重规则：同一 id 优先保留首次加载的（新路径优先）。
    pub async fn load_skills_by_category(
        &self,
        category: crate::types::verification::SkillCategory,
    ) -> Result<Vec<VerificationAsset>, TaijiError> {
        use crate::types::verification::SkillCategory;
        let dirs: &[&str] = match category {
            SkillCategory::Verify => &["yin/skills/verify"],
            SkillCategory::Converge => &["yin/skills/converge"],
            // orch/exec 尚未资产化，留空（P2 阶段填充）
            _ => &[],
        };
        let mut skills = Vec::new();
        for dir_name in dirs {
            let dir = self.data_dir.join(dir_name);
            if !dir.exists() {
                continue;
            }
            let mut read_dir = fs::read_dir(&dir).await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!("failed to read {:?} directory: {e}", dir),
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
                            Ok(Some(v)) if v.status == "active" => {
                                if !skills.iter().any(|existing: &VerificationAsset| existing.id == v.id) {
                                    skills.push(v);
                                }
                            }
                            Ok(Some(v)) => {
                                tracing::debug!(
                                    id = %v.id,
                                    status = %v.status,
                                    "load_skills_by_category: skipping non-active asset"
                                );
                            }
                            Ok(None) => {}
                            Err(e) => {
                                tracing::warn!("failed to load skill asset {:?}: {e}", path);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("error reading skills directory entry: {e}");
                    }
                }
            }
        }
        Ok(skills)
    }

    // -----------------------------------------------------------------------
    // V45 统一 Skill 资产（BCP §10.2 双轨——元层 ∪ 资产层加载）
    // -----------------------------------------------------------------------

    /// 类别 → 归藏子目录（V45 双轨：每 skill 一文件夹 `skills/{cat}/{id}/skill.yaml`）。
    fn skill_category_dir(category: crate::types::verification::SkillCategory) -> &'static str {
        use crate::types::verification::SkillCategory;
        match category {
            SkillCategory::Orch => "yang/skills/orch",
            SkillCategory::Exec => "yang/skills/exec",
            SkillCategory::Verify => "yin/skills/verify",
            SkillCategory::Converge => "yin/skills/converge",
        }
    }

    /// 旧 `VerificationAsset`（单文件 `*.yaml`）→ 新 `SkillAsset` 兼容转换。
    /// dual 按 check.kind 推导（旧格式无 dual 字段——种子库迁移期成形）。
    fn verification_to_skill_asset(
        v: &VerificationAsset,
    ) -> crate::types::verification::SkillAsset {
        use crate::types::verification::{
            CheckStats, SkillAsset, SkillImpl, SkillKind,
        };
        let implementations: Vec<SkillImpl> = v
            .checks
            .iter()
            .map(|c| SkillImpl {
                kind: SkillKind::from(c.kind),
                target: c.target.clone(),
                params: c.params.clone(),
                severity: Some(c.severity.clone()),
                pass_condition: c.pass_condition.clone(),
            })
            .collect();
        // dual 推导：旧资产无 dual 字段——按第一个 check kind 推导（与元层配对表一致）。
        let dual = v
            .checks
            .first()
            .map(|c| Self::dual_for_check_kind(c.kind.clone()))
            .unwrap_or_else(|| "".to_string());
        SkillAsset {
            id: v.id.clone(),
            name: v.name.clone(),
            description: v.description.clone(),
            tags: v.tags.clone(),
            examples: Vec::new(),
            input_modes: vec!["text".to_string()],
            output_modes: vec!["text".to_string()],
            category: None,
            dual,
            implementations,
            agent_target: v.agent_target.clone(),
            confidence: v.confidence,
            version: v.version,
            status: v.status.clone(),
            stats: CheckStats::default(), // 旧 check 级 stats 不迁移（种子重积累）
            env_tags: Vec::new(),
            parent_id: None,
            variant_of: None,
        }
    }

    /// check kind → 对偶元工具/skill id（旧格式迁移推导）。
    fn dual_for_check_kind(kind: crate::types::verification::CheckKind) -> String {
        use crate::types::verification::CheckKind;
        match kind {
            CheckKind::FileExists => "write".to_string(),
            CheckKind::SchemaValid => "read".to_string(),
            CheckKind::ReferenceResolves => "search".to_string(),
            CheckKind::CommandSucceeds => "bash".to_string(),
            CheckKind::TraceConsistency => "webfetch".to_string(),
            CheckKind::LlmJudgement => "recursive-decompose".to_string(),
        }
    }

    /// 加载统一 Skill 资产（V45 双轨——资产层；新文件夹格式 + 旧单文件兼容）。
    ///
    /// 返回 `active` 资产；同 id 新格式优先、旧格式去重。category 由子目录决定。
    pub async fn load_skill_assets(
        &self,
        category: crate::types::verification::SkillCategory,
    ) -> Result<Vec<crate::types::verification::SkillAsset>, TaijiError> {
        use crate::types::verification::SkillAsset;
        let dir = self.data_dir.join(Self::skill_category_dir(category));
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut skills: Vec<SkillAsset> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        // ── 新格式：skills/{cat}/{id}/skill.yaml ──
        let mut read_dir = fs::read_dir(&dir).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to read skill directory {:?}: {e}", dir),
            }
        })?;
        let mut subdirs: Vec<PathBuf> = Vec::new();
        let mut legacy_files: Vec<PathBuf> = Vec::new();
        while let Some(entry) = read_dir.next_entry().await.transpose() {
            let e = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("error reading {:?} entry: {e}", dir);
                    continue;
                }
            };
            let path = e.path();
            if path.is_dir() {
                subdirs.push(path);
            } else if path.extension().is_some_and(|x| x == "yaml")
                && path
                    .file_name()
                    .is_none_or(|n| !n.to_string_lossy().ends_with(".tmp"))
            {
                legacy_files.push(path);
            }
        }

        // 新文件夹格式优先加载。
        for subdir in &subdirs {
            let sf = subdir.join("skill.yaml");
            if !sf.exists() {
                continue;
            }
            match fs::read_to_string(&sf).await {
                Ok(content) => match serde_yaml::from_str::<SkillAsset>(&content) {
                    Ok(mut s) => {
                        if s.status != "active" {
                            tracing::debug!(id = %s.id, status = %s.status, "skipping non-active skill");
                            continue;
                        }
                        if s.category.is_none() {
                            s.category = Some(category);
                        }
                        seen.insert(s.id.clone());
                        skills.push(s);
                    }
                    Err(e) => {
                        tracing::warn!("failed to parse skill.yaml {:?}: {e}", sf);
                    }
                },
                Err(e) => {
                    tracing::warn!("failed to read skill.yaml {:?}: {e}", sf);
                }
            }
        }

        // 旧单文件格式（兼容——与 id 去重，新格式已注册的 id 跳过）。
        for path in &legacy_files {
            match self.load_verification_from_path(path).await {
                Ok(Some(v)) if v.status == "active" => {
                    if seen.contains(&v.id) {
                        continue;
                    }
                    seen.insert(v.id.clone());
                    skills.push(Self::verification_to_skill_asset(&v));
                }
                Ok(Some(v)) => {
                    tracing::debug!(id = %v.id, status = %v.status, "skipping non-active legacy skill");
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!("failed to load legacy skill asset {:?}: {e}", path);
                }
            }
        }

        Ok(skills)
    }

    /// 持久化 Skill 资产（V45 文件夹结构 `skills/{cat}/{id}/skill.yaml`；atomic write + version++）。
    /// dual 校验在合并视图域：资产层（同互补类别）∪ 元层（[`crate::infra::meta_skills`]）必须存在目标。
    pub async fn save_skill(
        &self,
        skill: &mut crate::types::verification::SkillAsset,
    ) -> Result<(), TaijiError> {
        use crate::types::verification::SkillCategory;
        let category = skill
            .effective_category()
            .ok_or_else(|| TaijiError::KnowledgeStoreUnavailable {
                context: format!("skill {} 缺少可推导 category", skill.id),
            })?;
        let cat_dir = self.data_dir.join(Self::skill_category_dir(category));
        let skill_dir = cat_dir.join(&skill.id);
        let path = skill_dir.join("skill.yaml");

        // version++（读现存文件）。
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path).await {
                if let Ok(existing) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                    let cur = existing
                        .get("version")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    skill.version = cur + 1;
                } else {
                    skill.version = 1;
                }
            } else {
                skill.version = 1;
            }
        } else {
            skill.version = 1;
        }

        // dual 校验（合并视图域）：目标必须存在，且 effective_category 与本 skill 类别互补。
        // 互补表：Orch↔Converge、Exec↔Verify（含 causal-verify 桥）。
        let dual_s = match crate::infra::meta_skills::meta_skill(&skill.dual) {
            Some(s) => Some(s),
            None => {
                // 资产层：扫两类互补目录（converge 的 dual 在 orch；verify 的 dual 在 exec）
                let c1 = match category {
                    SkillCategory::Orch | SkillCategory::Converge => SkillCategory::Orch,
                    SkillCategory::Exec | SkillCategory::Verify => SkillCategory::Exec,
                };
                let c2 = match category {
                    SkillCategory::Orch | SkillCategory::Converge => SkillCategory::Converge,
                    SkillCategory::Exec | SkillCategory::Verify => SkillCategory::Verify,
                };
                let mut found = None;
                for c in [c1, c2] {
                    if let Some(s) = self
                        .load_skill_assets(c)
                        .await?
                        .into_iter()
                        .find(|s| s.id == skill.dual)
                    {
                        found = Some(s);
                        break;
                    }
                }
                found
            }
        };
        let Some(dual_s) = dual_s else {
            return Err(TaijiError::KnowledgeStoreUnavailable {
                context: format!(
                    "skill {} 的 dual '{}' 不存在（合并视图域：元层 ∪ 资产层均无）",
                    skill.id, skill.dual
                ),
            });
        };
        let dual_cat = dual_s.effective_category();
        let complementary = matches!(
            (category, dual_cat),
            (SkillCategory::Orch, Some(SkillCategory::Converge))
                | (SkillCategory::Converge, Some(SkillCategory::Orch))
                | (SkillCategory::Exec, Some(SkillCategory::Verify))
                | (SkillCategory::Verify, Some(SkillCategory::Exec))
        );
        if !complementary {
            return Err(TaijiError::KnowledgeStoreUnavailable {
                context: format!(
                    "skill {} (category={:?}) 的 dual '{}' (category={:?}) 类别不互补",
                    skill.id, category, skill.dual, dual_cat
                ),
            });
        }

        fs::create_dir_all(&skill_dir).await.map_err(|e| {
            TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to create skill dir {:?}: {e}", skill_dir),
            }
        })?;

        let yaml = serde_yaml::to_string(&*skill).map_err(|e| TaijiError::KnowledgeStoreUnavailable {
            context: format!("failed to serialise skill {}: {e}", skill.id),
        })?;
        let tmp = path.with_extension("yaml.tmp");
        {
            let mut f = fs::File::create(&tmp).await.map_err(|e| TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to create skill tmp {:?}: {e}", tmp),
            })?;
            f.write_all(yaml.as_bytes()).await.map_err(|e| TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to write skill tmp {:?}: {e}", tmp),
            })?;
            f.flush().await.map_err(|e| TaijiError::KnowledgeStoreUnavailable {
                context: format!("failed to flush skill tmp {:?}: {e}", tmp),
            })?;
        }
        fs::rename(&tmp, &path).await.map_err(|e| TaijiError::KnowledgeStoreUnavailable {
            context: format!("failed to rename skill tmp {:?}: {e}", path),
        })?;
        Ok(())
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
    Skill(LegacyToolSkillAsset),
    #[serde(rename = "prompt")]
    Prompt(PromptAsset),
    #[serde(rename = "verification")]
    Verification(VerificationAsset),
}

impl CognitiveAsset {
    /// Return the asset type string (`"model"`, `"skill"`, `"prompt"`, `"verification"`).
    ///
    /// V43：Verification 变体优先用内部 `asset_type` 字段（可指向
    /// `yin_skill_verify` / `yin_skill_converge` 等细分目录），空时回退 `"verification"`。
    pub fn asset_type(&self) -> String {
        match self {
            CognitiveAsset::Model(_) => "model".into(),
            CognitiveAsset::Skill(_) => "skill".into(),
            CognitiveAsset::Prompt(p) => {
                if p.asset_type.is_empty() || p.asset_type == "prompt" {
                    "prompt".into()
                } else {
                    p.asset_type.clone()
                }
            }
            CognitiveAsset::Verification(v) => {
                if v.asset_type.is_empty() || v.asset_type == "verification" {
                    "verification".into()
                } else {
                    v.asset_type.clone()
                }
            }
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

/// V44：把既有 `{model_key}/` 分区资产合并回根级（BCP §10.1 去分区化）——幂等。
///
/// 迁移对象：knowledge 根下所有子目录（每个子目录视为一个旧分区），
/// 将其中资产层（yang/ yin/ models/ 等）合并回根。
/// 幂等规则：根级已存在同名目录/文件 → 跳过（可重复调用）；两者都不存在
/// → 跳过；仅分区有 → 移动（copy 后删源，兼容跨设备）。移动失败 → Err 上抛
/// （带路径，诊断性——无降级原则：迁移是数据完整性操作，不允许静默吞错）。
///
/// 调用时机：`build_engine`（所有命令入口）在 `GuizangClient::new` 之后调用一次。
pub async fn migrate_from_partitioned(root: &Path) -> Result<(), TaijiError> {
    const ASSET_LAYERS: [&str; 5] = ["yang", "yin", "models", "skills", "prompts"];
    let mut read_dir = fs::read_dir(root).await.map_err(|e| {
        TaijiError::KnowledgeStoreUnavailable {
            context: format!(
                "migrate_from_partitioned: failed to read knowledge root {:?}: {e}",
                root
            ),
        }
    })?;
    while let Some(entry) = read_dir.next_entry().await.transpose() {
        let Ok(entry) = entry else { continue };
        // 只处理目录型分区候选；根级白名单目录（model_stats.yaml 等文件忽略）。
        let Ok(ft) = entry.file_type().await else { continue };
        if !ft.is_dir() {
            continue;
        }
        let partition_dir = entry.path();
        let Some(dir_name) = partition_dir.file_name().map(|n| n.to_string_lossy().into_owned())
        else {
            continue;
        };
        // 跳过根级资产目录本身（非分区候选）。
        if ASSET_LAYERS.contains(&dir_name.as_str()) {
            continue;
        }

        for layer in ASSET_LAYERS {
            let src = partition_dir.join(layer);
            let dst = root.join(layer);
            let src_exists = fs::metadata(&src).await.is_ok();
            let dst_exists = fs::metadata(&dst).await.is_ok();
            if !src_exists {
                continue;
            }
            if dst_exists {
                // 目标已存在：逐文件幂等合并（copy，不覆盖已有）。
                merge_dir_recursive(src, dst).await?;
            } else {
                fs::rename(&src, &dst).await.map_err(|e| {
                    TaijiError::KnowledgeStoreUnavailable {
                        context: format!(
                            "migrate_from_partitioned: failed to move {:?} → {:?}: {e}",
                            src, dst
                        ),
                    }
                })?;
                tracing::info!(
                    layer,
                    partition = %dir_name,
                    "merged legacy partition asset layer into knowledge root"
                );
            }
        }

        // 分区目录已空（所有资产层已移出）→ 删除；非空（有其他文件）→ 保留。
        let _ = fs::remove_dir(&partition_dir).await;
    }

    Ok(())
}

/// 递归合并目录（目标已有同名文件 → 跳过不覆盖；仅源存在 → 复制后删源 = 移动）。
/// 递归经 `Box::pin` 打破 async 递归大小约束（E0733）。
async fn merge_dir_recursive(src: PathBuf, dst: PathBuf) -> Result<(), TaijiError> {
    fs::create_dir_all(&dst).await.map_err(|e| {
        TaijiError::KnowledgeStoreUnavailable {
            context: format!(
                "migrate_from_partitioned: failed to create {:?}: {e}",
                dst
            ),
        }
    })?;
    let mut read_dir = fs::read_dir(&src).await.map_err(|e| {
        TaijiError::KnowledgeStoreUnavailable {
            context: format!(
                "migrate_from_partitioned: failed to read {:?}: {e}",
                src
            ),
        }
    })?;
    while let Some(entry) = read_dir.next_entry().await.transpose() {
        let Ok(entry) = entry else { continue };
        let child_src = entry.path();
        let child_dst = dst.join(entry.file_name());
        let Ok(ft) = entry.file_type().await else { continue };
        if ft.is_dir() {
            Box::pin(merge_dir_recursive(child_src, child_dst)).await?;
        } else if !fs::metadata(&child_dst).await.is_ok() {
            fs::copy(&child_src, &child_dst).await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!(
                        "migrate_from_partitioned: failed to copy {:?} → {:?}: {e}",
                        child_src, child_dst
                    ),
                }
            })?;
            // 移动语义：复制成功后删除源文件（迁移 = 合并回根）。
            fs::remove_file(&child_src).await.map_err(|e| {
                TaijiError::KnowledgeStoreUnavailable {
                    context: format!(
                        "migrate_from_partitioned: failed to remove source {:?}: {e}",
                        child_src
                    ),
                }
            })?;
        }
    }
    // 源目录处理完毕 → 删除（仅当空；冲突保留文件时静默失败不影响迁移）。
    let _ = fs::remove_dir(&src).await;
    Ok(())
}

/// V39 种子复制结果报告。
#[derive(Debug, Clone, Default)]
pub struct SeedReport {
    /// 实际复制到根级资产树的资产数（V44）。
    pub copied: usize,
    /// 目标已存在而跳过的资产数（幂等）。
    pub skipped: usize,
    /// 源中 status=pruned 而不复制的资产数。
    pub pruned_skipped: usize,
}

/// V42 归藏目录 yang/yin 迁移（BCP §10.1）——幂等，可重跑：
/// - `prompts/*.yaml` → 按 agent_target 分派：
///   `"FittingAgent"` → `yang/prompts/`，`"CausalAgent"` → `yin/prompts/`
/// - `verifications/*.yaml` → `yin/skills/verify/`（V43：verifications 概念已废弃）
/// - `yin/verifications/*.yaml` → `yin/skills/verify/`（V43：迁移过渡目录）
/// - models/ 不迁移（根级统一存放，无需 yang/yin 拆分）
/// - 目标已存在同名文件 → 跳过，不覆盖
/// V42 归藏目录 yang/yin 迁移（BCP §10.1）——幂等，可重跑。
/// V44：改为根级处理（不再遍历模型分区——分区已合并回根）。
/// - `prompts/*.yaml` → 按 agent_target 分派：
///   `"CausalAgent"` → `yin/prompts/`，其余 → `yang/prompts/`
/// - `verifications/*.yaml` → `yin/skills/verify/`（V43：verifications 概念已废弃）
/// - `yin/verifications/*.yaml` → `yin/skills/verify/`（V43：迁移过渡目录）
/// - models/ 不迁移（无需 yang/yin 拆分）
pub async fn migrate_to_yang_yin(root: &Path) -> Result<(), TaijiError> {
    // 迁移 prompts/（根级）
    let old_prompts = root.join("prompts");
    if old_prompts.exists() {
        let yang_dir = root.join("yang/prompts");
        let yin_dir = root.join("yin/prompts");
        fs::create_dir_all(&yang_dir).await.map_err(TaijiError::IO)?;
        fs::create_dir_all(&yin_dir).await.map_err(TaijiError::IO)?;

        let mut r = fs::read_dir(&old_prompts).await.map_err(TaijiError::IO)?;
        while let Some(f) = r.next_entry().await.transpose() {
            let Ok(f) = f else { continue };
            let fp = f.path();
            if fp.extension().is_none_or(|e| e != "yaml") { continue; }
            let Some(name) = fp.file_name() else { continue };

            // 解析 agent_target 决定目标目录
            let target_dir = match fs::read_to_string(&fp).await {
                Ok(content) => {
                    if let Ok(val) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                        match val.get("agent_target").and_then(|v| v.as_str()) {
                            Some("CausalAgent") => &yin_dir,
                            _ => &yang_dir, // FittingAgent / 空 / 其他 → 阳轨
                        }
                    } else {
                        &yang_dir
                    }
                }
                Err(_) => &yang_dir,
            };

            let dst = target_dir.join(name);
            if !dst.exists() {
                if let Err(e) = fs::rename(&fp, &dst).await {
                    tracing::warn!(
                        file = %fp.display(),
                        error = %e,
                        "migrate_to_yang_yin: rename prompts failed, copying instead"
                    );
                    fs::copy(&fp, &dst).await.map_err(TaijiError::IO)?;
                }
            }
        }
    }

    // V43: 迁移 verifications/ + yin/verifications/ → yin/skills/verify/（BCP §10.1）
    // verifications 概念已废弃——统一收敛到 yin/skills/verify/。
    let yin_verify_dir = root.join("yin/skills/verify");
    fs::create_dir_all(&yin_verify_dir).await.map_err(TaijiError::IO)?;
    for old_dir in ["verifications", "yin/verifications"] {
        let old = root.join(old_dir);
        if !old.exists() { continue; }
        let mut r = fs::read_dir(&old).await.map_err(TaijiError::IO)?;
        while let Some(f) = r.next_entry().await.transpose() {
            let Ok(f) = f else { continue };
            let fp = f.path();
            if fp.extension().is_none_or(|e| e != "yaml") { continue; }
            let Some(name) = fp.file_name() else { continue };
            let dst = yin_verify_dir.join(name);
            if !dst.exists() {
                if let Err(e) = fs::rename(&fp, &dst).await {
                    tracing::warn!(
                        file = %fp.display(),
                        error = %e,
                        "migrate_to_yang_yin: rename {old_dir} failed, copying"
                    );
                    fs::copy(&fp, &dst).await.map_err(TaijiError::IO)?;
                }
            }
        }
    }

    tracing::info!("migrate_to_yang_yin: completed");
    Ok(())
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

/// V44 种子复制（BCP §10.1 去分区化）——从指定旧分区目录把活跃种子资产
/// （`prompts/` + `verifications/`，status != "pruned"）文件级复制回根级。
///
/// - 目标根级资产层自动创建。
/// - **不复制** `models/`（贝叶斯后验 = 学习单元累积，新单元从零开始——
///   复制旧统计会污染路由 UCB）。
/// - version 保持原值（种子 = 内容快照，非演化写；目标不存在同名文件）。
/// - 幂等：目标已存在同名资产 → 跳过不覆盖。
/// - 源分区缺失 → Err 上抛（无降级原则）；单资产文件损坏 → warn 跳过。
///
/// # Errors
/// 分区键非法 / 源分区缺失 → `TaijiError::KnowledgeStoreUnavailable`。
///
/// 调用方：`taiji seed <source_key>`（main.rs cmd_seed，V44 去分区后从旧分区恢复种子）。
pub async fn seed_partition(
    root: &Path,
    source_key: &str,
) -> Result<SeedReport, TaijiError> {
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

    let mut report = SeedReport::default();

    // 复制范围：prompts（yang/yin 对偶 + 旧兼容）+ verify Skill（活跃种子资产，V43）。
    // 源目录：yang/prompts + yin/prompts + prompts（提示词）；yin/skills/verify + yin/verifications（验证 Skill）。
    let seed_layers: [(&str, &str); 6] = [
        ("yang_prompt", "yang/prompts"),
        ("yin_prompt", "yin/prompts"),
        ("prompt", "prompts"),
        ("yin_skill_verify", "yin/skills/verify"),
        ("yin_verification", "yin/verifications"),
        ("verification", "verifications"),
    ];
    for (_type_, layer) in seed_layers {
        let src_layer = source_dir.join(layer);
        if !fs::metadata(&src_layer).await.map(|m| m.is_dir()).unwrap_or(false) {
            continue;
        }
        let dst_layer = root.join(layer);
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
                        file = %file_name,
                        "seeded asset into knowledge root (V44)"
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
        let root = GuizangClient::new(&dir).await.unwrap();
        // V44：根级资产树（单一资产树，BCP §10.1）
        assert!(dir.exists());
        assert!(dir.join("models").exists());
        assert!(dir.join("yang/prompts").exists());
        assert!(dir.join("yang/skills/orch").exists());
        assert!(dir.join("yang/skills/exec").exists());
        assert!(dir.join("yin/prompts").exists());
        assert!(dir.join("yin/skills/verify").exists());
        assert!(dir.join("yin/skills/converge").exists());
        // 不再创建分区目录（V44 去分区化）
        assert!(!dir.join("deepseek-deepseek-chat").exists());
        assert!(!dir.join("verifications").exists());
        assert!(!dir.join("truths").exists());
        assert!(!dir.join("index.yaml").exists());
        cleanup(&dir).await;
    }

    // ── V44 去分区化（BCP §10.1）──────────────────────────────────

    #[tokio::test]
    async fn test_single_asset_tree_root_writes() {
        let dir = test_dir("single_tree").await;
        let root = GuizangClient::new(&dir).await.unwrap();

        // 根 client 写根资产（V44：无分区，写入根级资产树）
        let mut prompt = crate::types::agent::PromptAsset::new(
            "root-prompt",
            "根提示词",
            "root",
            "content",
            "FittingAgent",
            vec!["general".into()],
        );
        root.save_prompt(&mut prompt).await.unwrap();

        // 资产落根级 yang/prompts（agent_target=FittingAgent）
        assert!(dir.join("yang/prompts/root-prompt.yaml").exists());
        let loaded = root.load_asset("yang_prompt", "root-prompt").await;
        assert!(loaded.is_ok(), "root asset loadable from root client");

        // 检索根级可见
        let hits = root.search_prompts(&["general"]).await.unwrap();
        assert!(hits.iter().any(|x| x.id == "root-prompt"));

        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_migrate_from_partitioned_idempotent() {
        let dir = test_dir("migrate_back").await;
        let root = GuizangClient::new(&dir).await.unwrap();

        // 模拟旧分区布局：{model_key}/yang/prompts/ + models/
        let part = dir.join("deepseek-deepseek-chat");
        let part_yang_prompts = part.join("yang/prompts");
        fs::create_dir_all(&part_yang_prompts).await.unwrap();
        fs::write(
            part_yang_prompts.join("legacy-prompt.yaml"),
            "id: legacy-prompt\ntype: prompt\nname: 旧提示词\ndescription: test\nlayer: 1\ntags: [legacy]\nconfidence: 0.9\nversion: 1\ncontent: content\nagent_target: FittingAgent\n",
        )
        .await
        .unwrap();
        let part_models = part.join("models");
        fs::create_dir_all(&part_models).await.unwrap();
        fs::write(part_models.join("m.yaml"), "id: m\n").await.unwrap();

        // 首次合并：分区资产层移回根
        migrate_from_partitioned(&dir).await.unwrap();
        assert!(dir.join("yang/prompts/legacy-prompt.yaml").exists());
        assert!(dir.join("models/m.yaml").exists());
        assert!(!part.join("yang/prompts/legacy-prompt.yaml").exists());

        // 幂等：重复调用无操作不报错
        migrate_from_partitioned(&dir).await.unwrap();

        // 根 client 可读迁移后的资产
        let loaded = root.load_asset("yang_prompt", "legacy-prompt").await;
        assert!(loaded.is_ok(), "migrated asset readable from root");

        cleanup(&dir).await;
    }

    // ── V39 种子复制（taiji seed）────────────────────────────────────

    #[tokio::test]
    async fn test_seed_partition_copies_active_seeds_and_skips_pruned() {
        let dir = test_dir("seed_copy").await;
        let root = GuizangClient::new(&dir).await.unwrap();

        // 源分区：一个 active prompt + 一个 pruned prompt + 一个 active verification
        // （V44：源 = 旧 {model_key}/ 分区目录，目标 = 根级资产树）
        let src = dir.join("deepseek-deepseek-src");
        let src_prompts = src.join("prompts");
        fs::create_dir_all(&src_prompts).await.unwrap();
        let write_prompt = |path: &std::path::Path, id: &str, status: &str| {
            let yaml = format!(
                "id: {id}\ntype: prompt\nname: {id}\ndescription: test\nstatus: {status}\nlayer: 1\nconfidence: 0.9\nversion: 1\nagent_target: FittingAgent\ntags: [general]\ncontent: content\n"
            );
            std::fs::write(path, yaml).unwrap();
        };
        write_prompt(&src_prompts.join("seed-prompt.yaml"), "seed-prompt", "active");
        write_prompt(&src_prompts.join("pruned-prompt.yaml"), "pruned-prompt", "pruned");
        let src_verifications = src.join("verifications");
        fs::create_dir_all(&src_verifications).await.unwrap();
        fs::write(
            src_verifications.join("seed-verification.yaml"),
            "id: seed-verification\ntype: verification\nname: 种子契约\ndescription: test\nstatus: active\nlayer: 1\nconfidence: 0.9\nversion: 1\ntags: [general]\nchecks: []\n",
        )
        .await
        .unwrap();

        // 根级已存在同名资产（幂等跳过测试用）
        fs::create_dir_all(dir.join("prompts")).await.unwrap();
        fs::write(
            dir.join("prompts/seed-prompt.yaml"),
            "id: seed-prompt\ntype: prompt\nname: 已存在\ndescription: test\nstatus: active\nlayer: 1\nconfidence: 0.9\nversion: 1\nagent_target: FittingAgent\ntags: [general]\ncontent: content\n",
        )
        .await
        .unwrap();

        let report = seed_partition(&dir, "deepseek-deepseek-src").await.unwrap();
        // 复制：seed-verification（seed-prompt 被根级已存在跳过，pruned 排除）
        assert_eq!(report.copied, 1);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.pruned_skipped, 1);

        // 幂等：二次调用全部跳过
        let report2 = seed_partition(&dir, "deepseek-deepseek-src").await.unwrap();
        assert_eq!(report2.copied, 0);
        assert!(report2.skipped >= 2);

        // pruned 未复制；models/ 不复制
        assert!(!dir.join("prompts/pruned-prompt.yaml").exists());
        assert!(!dir.join("models/seed-verification.yaml").exists());
        // 根级验证资产可见
        assert!(dir.join("verifications/seed-verification.yaml").exists());

        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_seed_partition_missing_source_errors() {
        let dir = test_dir("seed_missing_source").await;
        GuizangClient::new(&dir).await.unwrap();

        let err = seed_partition(&dir, "deepseek-no-such-model").await.unwrap_err();
        assert!(err.to_string().contains("source partition"), "{err}");

        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_seed_partition_invalid_key_errors() {
        let dir = test_dir("seed_invalid_key").await;
        GuizangClient::new(&dir).await.unwrap();

        // 路径穿越 / 非法字符一律拒绝（CLI 输入即攻击面）。
        for bad in ["../evil", "a/b", "a\\b", "a b", "a.b", ""] {
            let err = seed_partition(&dir, bad).await.unwrap_err();
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
        let root = GuizangClient::new(&dir).await.unwrap();

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

        // V44：单一资产树——model_stats 根级可见（无分区派生）
        let stats2 = root.load_model_stats().await.unwrap();
        assert_eq!(stats2.get("deepseek-deepseek-chat").unwrap().n, 3);

        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_health_check_ok() {
        let dir = test_dir("health_check_ok").await;
        let client = GuizangClient::new(&dir).await.unwrap();
        assert!(client.health_check().is_ok());
        cleanup(&dir).await;
    }

    #[tokio::test]
    async fn test_save_and_load_skill() {
        let dir = test_dir("save_load_skill").await;
        let client = GuizangClient::new(&dir).await.unwrap();

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
        let client = GuizangClient::new(&dir).await.unwrap();

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
        let client = GuizangClient::new(&dir).await.unwrap();

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
        let client = GuizangClient::new(&dir).await.unwrap();

        let result = client.load_asset("prompt", "nonexistent").await;
        assert!(result.is_err());

        cleanup(&dir).await;
    }

    // ── Prompt asset tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_save_and_load_prompt() {
        let dir = test_dir("save_load_prompt").await;
        let client = GuizangClient::new(&dir).await.unwrap();

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
        let client = GuizangClient::new(&dir).await.unwrap();

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
        let client = GuizangClient::new(&dir).await.unwrap();

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
        // V41：new() 不再创建资产层——测试手写文件需自建目录（模拟真实分区资产）。
        let prompts_dir = dir.join("prompts");
        fs::create_dir_all(&prompts_dir).await.unwrap();
        let path = prompts_dir.join("hand-written-prompt.yaml");
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

    // ── V45 save_skill dual 校验 ──

    #[tokio::test]
    async fn test_save_skill_valid_dual_writes_folder() {
        use crate::types::verification::{
            CheckSeverity, SkillAsset, SkillImpl, SkillKind,
        };
        let dir = std::env::temp_dir().join(format!("taiji_save_skill_ok_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let guizang = GuizangClient::new(&dir).await.unwrap();
        // file-exists（阴）dual=write（阳元层）→ 校验通过。
        let mut skill = SkillAsset {
            id: "file-exists".into(),
            name: "交付物存在性".into(),
            description: "test".into(),
            tags: vec![],
            examples: vec![],
            input_modes: vec!["text".into()],
            output_modes: vec!["text".into()],
            category: None,
            dual: "write".into(),
            implementations: vec![SkillImpl {
                kind: SkillKind::FileExists,
                target: "deliverables/*".into(),
                params: serde_json::json!({}),
                severity: Some(CheckSeverity::Hard),
                pass_condition: "产物存在".into(),
            }],
            agent_target: String::new(),
            confidence: 0.8,
            version: 0,
            status: "active".into(),
            stats: crate::types::verification::CheckStats::default(),
            env_tags: vec![],
            parent_id: None,
            variant_of: None,
        };
        guizang.save_skill(&mut skill).await.expect("valid dual saves");
        assert_eq!(skill.version, 1, "首次写 version=1");
        let path = dir.join("yin/skills/verify/file-exists/skill.yaml");
        assert!(path.exists(), "应写入文件夹格式 {:?}", path);
        // 重读。
        let loaded = guizang.load_skill_assets(crate::types::verification::SkillCategory::Verify).await.unwrap();
        assert!(loaded.iter().any(|s| s.id == "file-exists"), "读回成功");
        // load_all_verifications 应能读到文件夹 skill.yaml（DMN 桥）
        let verifs = guizang.load_all_verifications().await.unwrap();
        assert!(
            verifs.iter().any(|v| v.id == "file-exists"),
            "load_all_verifications 应桥接 SkillAsset → VerificationAsset"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_save_skill_invalid_dual_rejected() {
        use crate::types::verification::{
            CheckSeverity, SkillAsset, SkillImpl, SkillKind,
        };
        let dir = std::env::temp_dir().join(format!("taiji_save_skill_bad_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let guizang = GuizangClient::new(&dir).await.unwrap();
        let mut skill = SkillAsset {
            id: "bogus-skill".into(),
            name: "测试".into(),
            description: "test".into(),
            tags: vec![],
            examples: vec![],
            input_modes: vec!["text".into()],
            output_modes: vec!["text".into()],
            category: None,
            dual: "nonexistent-dual".into(),
            implementations: vec![SkillImpl {
                kind: SkillKind::FileExists,
                target: "deliverables/*".into(),
                params: serde_json::json!({}),
                severity: Some(CheckSeverity::Hard),
                pass_condition: "x".into(),
            }],
            agent_target: String::new(),
            confidence: 0.5,
            version: 0,
            status: "active".into(),
            stats: crate::types::verification::CheckStats::default(),
            env_tags: vec![],
            parent_id: None,
            variant_of: None,
        };
        let err = guizang.save_skill(&mut skill).await;
        assert!(err.is_err(), "dual 不存在应拒绝保存");
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("nonexistent-dual"), "err 应含 dual id: {}", msg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// V45：空知识库 load_all_verifications 元层保底（DMN 冷启动）。
    #[tokio::test]
    async fn test_load_all_verifications_meta_fallback() {
        let dir = std::env::temp_dir().join(format!("taiji_verif_meta_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let guizang = GuizangClient::new(&dir).await.unwrap();
        let verifs = guizang.load_all_verifications().await.unwrap();
        assert!(
            !verifs.is_empty(),
            "空库 load_all_verifications 应由元层保底"
        );
        assert!(
            verifs.iter().any(|v| v.id == "file-exists"),
            "元层 file-exists 应可见"
        );
        // command-succeeds 空 command 不应出现在 checks 里（已过滤）
        if let Some(cs) = verifs.iter().find(|v| v.id == "command-succeeds") {
            assert!(
                cs.checks.is_empty(),
                "空 command 的 CommandSucceeds 不应落入 VerificationAsset.checks"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
