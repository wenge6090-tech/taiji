//! 本体类型（BCP §6.6 本体挖掘）——连山纯符号挖掘的产物类型。
//!
//! Ontology = 词汇表（[`SemanticType`]）+ 拓扑（[`OntologyEdge`] type→type）+
//! 逻辑（[`OntologyRule`] type-level）。纯数据类型，零业务依赖。

use crate::types::verification::CheckSeverity;
use serde::{Deserialize, Serialize};

/// `ontology/types.yaml` 文件顶层结构（词汇表）。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SemanticTypeFile {
    #[serde(default)]
    pub types: Vec<SemanticType>,
}

/// 语义类型（词汇表层，§6.6）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticType {
    /// 唯一标识（如 `security-check`、`deploy-action`）。
    pub id: String,
    /// 人类可读名。
    pub name: String,
    /// 语义说明。
    pub description: String,
    /// 类型层级（taxonomy），None = 顶层。
    #[serde(default)]
    pub parent: Option<String>,
    /// 来源：人工种子 / 挖掘产出（未命名）/ 编译命名。
    #[serde(default = "default_source")]
    pub source: TypeSource,
}

fn default_source() -> TypeSource {
    TypeSource::Human
}

/// 语义类型来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeSource {
    /// 人工种子。
    Human,
    /// 连山挖掘产出（未命名，待 compile 命名）。
    Mined,
    /// 编译任务命名后固化。
    Compiled,
}

/// 任务语义视图（resolve_entity 实体链接输出，合并进 Meta compose 调用）。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TaskOntologyView {
    /// 领域：Security | Infra | Data | Finance | General。
    pub domain: String,
    /// 动作：Create | Read | Update | Delete | Debug | Fix。
    pub action: String,
    /// 涉及实体（AuthService / Database / Config ...）。
    #[serde(default)]
    pub objects: Vec<String>,
    /// 环境：Production | Staging | Dev（None = 无）。
    #[serde(default)]
    pub env: Option<String>,
    /// 是否安全/关键敏感任务。
    #[serde(default)]
    pub is_critical: bool,
}

/// 本体边类别（Forbid 留给 SafetyHook + 人工 rules.yaml，不挖掘——§6.6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OntologyEdgeKind {
    /// 软依赖：A 通常需要 B（可替代，类型级软查询）。
    WeakDependency,
    /// 时序：A 先于 B。
    Sequence,
}

/// 本体边（type→type——`from`/`to` 是 [`SemanticType`] id，非资产 id）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OntologyEdge {
    /// 源类型 id。
    pub from: String,
    /// 目标类型 id。
    pub to: String,
    pub kind: OntologyEdgeKind,
    /// 强度 = P(pass | a∧b) − P(pass | a)（lift）。
    pub strength: f64,
    /// 共现样本数（≥ min_samples 才产出）。
    pub samples: u64,
    /// 支撑此边的资产 id（审计）。
    #[serde(default)]
    pub evidence: Vec<String>,
}

/// 规则触发条件（domain × env × action 三者可选匹配，None = 不约束该维度）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RuleCondition {
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
}

/// 本体规则（type-level 逻辑约束，§6.6 逻辑层）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OntologyRule {
    pub id: String,
    /// 触发条件。
    pub when: RuleCondition,
    /// 必须存在的语义类型（缺失 → 硬约束违反）。
    #[serde(default)]
    pub require: Vec<String>,
    /// 禁止出现的语义类型（命中 → 硬约束违反）。
    #[serde(default)]
    pub forbid: Vec<String>,
    #[serde(default = "default_severity")]
    pub severity: CheckSeverity,
}

fn default_severity() -> CheckSeverity {
    CheckSeverity::Hard
}

/// 共现对（挖掘输入；经 `abstract_to_types` 从 id 级抽象为 type 级）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CooccurPair {
    pub a: String,
    pub b: String,
    /// 共现次数。
    pub co: u64,
    /// 共现且通过次数。
    pub pass: u64,
}

/// 失败分组（挖掘输入：check kind × env_tags）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FailureGroup {
    pub env_tags: Vec<String>,
    pub check_kind: String,
    pub fails: u64,
    pub total: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_type_yaml_roundtrip() {
        let f = SemanticTypeFile {
            types: vec![SemanticType {
                id: "security-check".into(),
                name: "安全合规检查".into(),
                description: "验证产出不引入安全漏洞".into(),
                parent: None,
                source: TypeSource::Human,
            }],
        };
        let yaml = serde_yaml::to_string(&f).unwrap();
        let back: SemanticTypeFile = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.types[0].id, "security-check");
    }

    #[test]
    fn task_ontology_view_default() {
        let v = TaskOntologyView::default();
        assert_eq!(v.domain, "");
        assert!(v.objects.is_empty());
        assert!(v.env.is_none());
    }
}
