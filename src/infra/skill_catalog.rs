//! V45 Skill 合并视图（AGENTS.md §9 双轨——元层 ∪ 资产层）。
//!
//! 加载源 = [`crate::infra::meta_skills`]（Rust 硬编码元层）
//!        ∪ [`GuizangClient::load_skill_assets`]（资产层 `skills/{cat}/{id}/skill.yaml`）
//! 同 id 资产优先（资产层覆盖元层教学字段）；`dual` 校验在合并视图域。

use crate::types::verification::{SkillAsset, SkillCategory, SkillKind};
use std::collections::HashMap;

/// 工具集路由画像（V45 AGENTS.md §9——弱模型最小集 vs 完整集）。
///
/// 默认 `Full`；已知弱模型在 [`factory::profile_for_model`] 映射为 `Minimal`
/// （隐藏 recursive-decompose/webfetch 等高代价工具，保 read/write/bash + search + 全部阴判据）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolProfile {
    /// 完整工具集（强模型 / 默认）。
    Full,
    /// 最小集（弱模型路由——隐藏高代价联网；阴判据保留，验证闭环不断）。
    Minimal,
}

/// 加载合并视图（元层 ∪ 资产层，同 id 资产优先）。
///
/// `profile` 过滤执行体：`Minimal` profile 隐藏 `Webfetch`（高代价联网；
/// V47 起不再隐藏 RecursiveDecompose——拆解正是弱模型规避超限的核心手段）；
/// 元层判据（Verify/Converge）不受 profile 影响。
pub async fn load_skill_catalog(
    guizang: &crate::infra::knowledge::GuizangClient,
    category: SkillCategory,
    profile: ToolProfile,
) -> Result<Vec<SkillAsset>, crate::infra::error::TaijiError> {
    // 元层（保底）。
    let mut merged: HashMap<String, SkillAsset> = crate::infra::meta_skills::meta_skills(category)
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();

    // 资产层覆盖（同 id 替换，新 id 追加）。
    for s in guizang.load_skill_assets(category).await? {
        merged.insert(s.id.clone(), s);
    }

    // profile 过滤（仅对阳面执行体）。
    let mut out: Vec<SkillAsset> = merged.into_values().collect();
    if matches!(profile, ToolProfile::Minimal) {
        out.retain(|s| !is_profile_hidden(s));
    }
    Ok(out)
}

/// Minimal profile 下隐藏的 Skill（弱模型路由）。
///
/// V47（AGENTS.md §9）：仅隐藏 webfetch（高代价联网）；recursive-decompose 不再
/// 隐藏——拆解正是弱模型（小上下文）规避上下文超限的核心手段。保留
/// read/write/bash + search 基础集——任务基础执行与验证闭环仍可用。
fn is_profile_hidden(s: &SkillAsset) -> bool {
    let cat = s.effective_category();
    // 仅过滤阳面执行体；阴面判据保留（验证闭环不能断）。
    if !matches!(cat, Some(SkillCategory::Orch) | Some(SkillCategory::Exec)) {
        return false;
    }
    // 仅隐藏高代价联网工具（V52：Builtin kind，builtin 名 = skill.id）。
    s.implementations
        .iter()
        .any(|i| matches!(i.kind, SkillKind::Builtin) && s.id == "webfetch")
}

/// 合并视图域 dual 解析：在已加载的 catalog 中查找对偶资产。
pub fn resolve_dual<'a>(catalog: &'a [SkillAsset], id: &str) -> Option<&'a SkillAsset> {
    catalog.iter().find(|s| s.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_profile_hidden_minimal() {
        // V47：recursive-decompose 不再隐藏（拆解是弱模型规避超限的手段）；
        // webfetch 隐藏；read 保留；阴判据全保留。
        let rd = meta_skill_by_id("recursive-decompose");
        assert!(!is_profile_hidden(&rd));
        let rd_w = meta_skill_by_id("webfetch");
        assert!(is_profile_hidden(&rd_w));
        let read = meta_skill_by_id("read");
        assert!(!is_profile_hidden(&read));
        let verify = meta_skill_by_id("file-exists");
        assert!(!is_profile_hidden(&verify));
    }

    fn meta_skill_by_id(id: &str) -> SkillAsset {
        crate::infra::meta_skills::meta_skill(id).unwrap_or_else(|| panic!("元层缺 {id}"))
    }

    #[tokio::test]
    async fn test_load_skill_catalog_meta_only_when_empty() {
        // 空知识库：元层保底，verify 类别应含全部 6 个元判据。
        let dir = tempfile_dir();
        let _g = TmpGuard(dir.clone());
        let guizang = crate::infra::knowledge::GuizangClient::new(&dir).await.expect("guizang init");
        let cat = load_skill_catalog(&guizang, SkillCategory::Verify, ToolProfile::Full)
            .await
            .expect("catalog load");
        assert_eq!(cat.len(), 6, "空知识库 verify 应由元层保底 6 个");
        // id 完整性。
        let ids: Vec<&str> = cat.iter().map(|s| s.id.as_str()).collect();
        for expected in ["file-exists", "schema-valid", "command-succeeds",
                        "reference-resolves", "trace-consistency", "semantic-coherence"]
        {
            assert!(ids.contains(&expected), "缺 {expected}");
        }
    }

    #[tokio::test]
    async fn test_load_skill_catalog_minimal_keeps_recursive_decompose() {
        let dir = tempfile_dir();
        let _g = TmpGuard(dir.clone());
        let guizang = crate::infra::knowledge::GuizangClient::new(&dir).await.expect("guizang init");
        let orch = load_skill_catalog(&guizang, SkillCategory::Orch, ToolProfile::Minimal)
            .await
            .expect("orch load");
        assert_eq!(
            orch.len(),
            1,
            "V47: minimal profile 保留 recursive-decompose → orch 1 个"
        );

        let orch_full = load_skill_catalog(&guizang, SkillCategory::Orch, ToolProfile::Full)
            .await
            .expect("orch full");
        assert_eq!(orch_full.len(), 1, "full profile orch = recursive-decompose 1 个");
    }

    fn tempfile_dir() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("taiji_skill_catalog_test_{}", n));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// RAII 清临时目录（AGENTS §5）。
    struct TmpGuard(std::path::PathBuf);
    impl Drop for TmpGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn test_load_skill_catalog_meta_only_when_empty_cleaned() {
        // 复用已有断言逻辑 + 确保清理（原测试 tempfile_dir 未 Drop 清理）。
        let dir = tempfile_dir();
        let _g = TmpGuard(dir.clone());
        let guizang = crate::infra::knowledge::GuizangClient::new(&dir).await.expect("guizang init");
        let cat = load_skill_catalog(&guizang, SkillCategory::Verify, ToolProfile::Full)
            .await
            .expect("catalog load");
        assert_eq!(cat.len(), 6);
    }
}