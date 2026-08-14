//! SkillTriggerEngine — L1 skill matching via regex + tags.
//! Runs before YangAgent creation to select tools for the agent to use.
//!
//! Matching strategy:
//!   1. Regex pass — match `task_description` against registered patterns
//!   2. Tag fallback — if no regex matches, use `task_type_tags` overlap
//!   3. Score sorting — descending by `match_weight`, capped at 1.0
//!
//! See AGENTS.md §5 for detailed rules.

use crate::types::agent::SkillRef;
use regex::Regex;
use std::collections::HashMap;

/// An entry in the trigger registry: a compiled regex pattern paired with
/// the skill it activates.
#[derive(Debug, Clone)]
pub struct TriggerEntry {
    /// Compiled regex pattern matched against the task description.
    pub pattern: Regex,
    /// The skill definition to activate on a match.
    pub skill: SkillRef,
}

/// Engine for matching task descriptions / tags to L1 skills.
///
/// Maintains a registry of regex-based trigger patterns mapped to skills.
/// When invoked, performs a two-pass matching strategy and returns the
/// top-k results sorted by match weight.
#[derive(Debug, Clone)]
pub struct SkillTriggerEngine {
    /// Ordered list of trigger entries (regex + skill pairs).
    triggers: Vec<TriggerEntry>,
    /// Tag-to-skill-ids mappings for fallback matching when regex yields no results.
    /// One tag can map to multiple skill IDs (1:N).
    tag_mappings: HashMap<String, Vec<String>>,
}

impl SkillTriggerEngine {
    /// Create a new engine pre-populated with default skills.
    ///
    /// Default skills cover the most common tool categories:
    ///   - File reading    (`read`)
    ///   - File writing    (`write`)
    ///   - Shell execution (`bash`)
    ///   - Web requests    (`webfetch`)
    ///   - Web search      (`search`)
    pub fn new() -> Self {
        let mut engine = Self {
            triggers: Vec::with_capacity(8),
            tag_mappings: HashMap::new(),
        };

        // Register default skills with their regex patterns and tag mappings.
        engine.register_skill_inner(
            r"read|search|find|grep|lookup",
            SkillRef {
                id: "read".into(),
                name: "文件读取".into(),
                tool_name: "read".into(),
                match_weight: 0.8,
summary: String::new(),
            },
            Some("read"),
        );

        engine.register_skill_inner(
            r"write|create|edit|modify|update",
            SkillRef {
                id: "write".into(),
                name: "文件写入".into(),
                tool_name: "write".into(),
                match_weight: 0.8,
summary: String::new(),
            },
            Some("write"),
        );

        engine.register_skill_inner(
            r"bash|shell|exec|run|command|terminal",
            SkillRef {
                id: "exec".into(),
                name: "命令执行".into(),
                tool_name: "bash".into(),
                match_weight: 0.8,
summary: String::new(),
            },
            Some("code"),
        );

        engine.register_skill_inner(
            r"web|url|fetch|http|api|curl",
            SkillRef {
                id: "web".into(),
                name: "网络请求".into(),
                tool_name: "webfetch".into(),
                match_weight: 0.7,
summary: String::new(),
            },
            Some("web"),
        );

        engine.register_skill_inner(
            r"search|google|research|find.*info",
            SkillRef {
                id: "search".into(),
                name: "网络搜索".into(),
                tool_name: "search".into(),
                match_weight: 0.7,
summary: String::new(),
            },
            Some("search"),
        );

        engine
    }

    /// Register a new trigger pattern with its associated skill.
    ///
    /// Returns immediately (with a warning) if the regex pattern fails
    /// to compile, so callers can rely on the engine always being in a
    /// valid state.
    pub fn register_skill(&mut self, pattern: &str, skill: SkillRef) {
        self.register_skill_inner(pattern, skill, None);
    }

    /// Match a task description (and optional tags) against registered skills.
    ///
    /// **First pass** — regex matching against `task_description`:
    ///   For each registered trigger whose pattern matches, the skill is
    ///   collected and its `match_weight` is boosted by 0.1 (capped at 1.0).
    ///
    /// **Fallback** — tag-based matching:
    ///   If no regex match is found, each `task_type_tag` is looked up in the
    ///   internal tag→skill mapping.  Matching skills are returned with their
    ///   base weight (no boost).
    ///
    /// **Result**: skills sorted by `match_weight` descending, limited to top 10.
    pub fn match_skills(
        &self,
        task_description: &str,
        task_type_tags: &[String],
    ) -> Vec<SkillRef> {
        let lower_desc = task_description.to_lowercase();

        // ---- Pass 1: regex matching ----
        let mut matched: Vec<SkillRef> = Vec::new();

        for entry in &self.triggers {
            if entry.pattern.is_match(&lower_desc) {
                let mut skill = entry.skill.clone();
                // Boost weight by 0.1, cap at 1.0
                skill.match_weight = (skill.match_weight + 0.1).min(1.0);
                matched.push(skill);
            }
        }

        // ---- Pass 2: tag-based fallback ----
        if matched.is_empty() {
            for tag in task_type_tags {
                let lower_tag = tag.to_lowercase();
                if let Some(skill_ids) = self.tag_mappings.get(&lower_tag) {
                    for skill_id in skill_ids {
                        if !matched.iter().any(|s| s.id == *skill_id) {
                            // Look up the base skill from the trigger registry.
                            if let Some(base) = self.triggers.iter().find(|e| e.skill.id == *skill_id) {
                                matched.push(base.skill.clone());
                            }
                        }
                    }
                }
            }
        }

        // Sort by match_weight descending, take top 10.
        matched.sort_by(|a, b| {
            b.match_weight
                .partial_cmp(&a.match_weight)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        matched.truncate(10);

        matched
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Register a trigger and optionally associate a tag for fallback matching.
    fn register_skill_inner(
        &mut self,
        pattern: &str,
        skill: SkillRef,
        tag: Option<&str>,
    ) {
        match Regex::new(pattern) {
            Ok(re) => {
                self.triggers.push(TriggerEntry {
                    pattern: re,
                    skill: skill.clone(),
                });
                if let Some(t) = tag {
                    self.tag_mappings
                        .entry(t.to_lowercase())
                        .or_default()
                        .push(skill.id.clone());
                }
                tracing::debug!(
                    pattern = %pattern,
                    skill_id = %skill.id,
                    skill_name = %skill.name,
                    "Registered skill trigger"
                );
            }
            Err(e) => {
                tracing::warn!(
                    pattern = %pattern,
                    skill_id = %skill.id,
                    error = %e,
                    "Failed to compile regex pattern for skill — skipping"
                );
            }
        }
    }
}

impl Default for SkillTriggerEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill_ref(id: &str, name: &str, tool: &str, weight: f64) -> SkillRef {
        SkillRef {
            id: id.into(),
            name: name.into(),
            tool_name: tool.into(),
            match_weight: weight,
summary: String::new(),
        }
    }

    #[test]
    fn test_new_engine_has_default_skills() {
        let engine = SkillTriggerEngine::new();
        // Default: 5 skills registered
        assert_eq!(engine.triggers.len(), 5);
    }

    #[test]
    fn test_match_read_skill_by_regex() {
        let engine = SkillTriggerEngine::new();
        let desc = "I need to read the configuration file";
        let results = engine.match_skills(desc, &[]);
        assert!(!results.is_empty());
        assert!(results.iter().any(|s| s.id == "read"));
        // Weight should be boosted from 0.8 to 0.9
        let read_skill = results.iter().find(|s| s.id == "read").unwrap();
        assert!((read_skill.match_weight - 0.9).abs() < 1e-6);
    }

    #[test]
    fn test_match_write_skill_by_regex() {
        let engine = SkillTriggerEngine::new();
        let desc = "edit the source file to fix the bug";
        let results = engine.match_skills(desc, &[]);
        assert!(results.iter().any(|s| s.id == "write"));
    }

    #[test]
    fn test_match_exec_skill_by_regex() {
        let engine = SkillTriggerEngine::new();
        let desc = "run the test suite with cargo";
        let results = engine.match_skills(desc, &[]);
        assert!(results.iter().any(|s| s.id == "exec"));
    }

    #[test]
    fn test_match_web_skill_by_regex() {
        let engine = SkillTriggerEngine::new();
        let desc = "fetch the API response from endpoint";
        let results = engine.match_skills(desc, &[]);
        assert!(results.iter().any(|s| s.id == "web"));
    }

    #[test]
    fn test_multiple_matches_get_sorted_by_weight() {
        let engine = SkillTriggerEngine::new();
        let desc = "search and read files then run commands";
        let results = engine.match_skills(desc, &[]);
        // Should have at least 3 matches
        assert!(results.len() >= 3);
        // Verify descending order
        for window in results.windows(2) {
            assert!(window[0].match_weight >= window[1].match_weight);
        }
    }

    #[test]
    fn test_weight_boost_capped_at_one() {
        let mut engine = SkillTriggerEngine::new();
        // Register a skill with weight already at 0.95
        engine.register_skill(
            r"boostme",
            skill_ref("boosted", "Boosted", "boost", 0.95),
        );
        let desc = "please boostme now";
        let results = engine.match_skills(desc, &[]);
        let boosted = results.iter().find(|s| s.id == "boosted").unwrap();
        assert!((boosted.match_weight - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_no_match_returns_empty() {
        let engine = SkillTriggerEngine::new();
        let desc = "completely unrelated nonsense";
        let results = engine.match_skills(desc, &[]);
        assert!(results.is_empty());
    }

    #[test]
    fn test_tag_fallback_when_no_regex_match() {
        let engine = SkillTriggerEngine::new();
        let desc = "some non-matching description";
        let tags: Vec<String> = vec!["code".into()];
        let results = engine.match_skills(desc, &tags);
        // "code" maps to the "exec" skill (bash)
        assert!(!results.is_empty());
        assert!(results.iter().any(|s| s.id == "exec"));
    }

    #[test]
    fn test_tag_fallback_does_not_duplicate() {
        let engine = SkillTriggerEngine::new();
        let tags: Vec<String> = vec!["code".into(), "code".into()];
        let results = engine.match_skills("no regex match", &tags);
        // Duplicate tag should not produce duplicate skill entries
        let exec_count = results.iter().filter(|s| s.id == "exec").count();
        assert_eq!(exec_count, 1);
    }

    #[test]
    fn test_tag_fallback_multiple_tags() {
        let engine = SkillTriggerEngine::new();
        let tags: Vec<String> = vec!["read".into(), "write".into()];
        let results = engine.match_skills("no regex match", &tags);
        assert!(results.iter().any(|s| s.id == "read"));
        assert!(results.iter().any(|s| s.id == "write"));
    }

    #[test]
    fn test_top_10_limit() {
        let mut engine = SkillTriggerEngine::new();
        // Register 15 skills all with the same catch-all pattern
        for i in 0..15 {
            let _ = engine.register_skill(
                r"catch.?all",
                skill_ref(
                    &format!("skill-{}", i),
                    &format!("Skill {}", i),
                    &format!("tool-{}", i),
                    0.5,
                ),
            );
        }
        let desc = "catch all of these";
        let results = engine.match_skills(desc, &[]);
        assert!(results.len() <= 10);
    }

    #[test]
    fn test_invalid_regex_pattern_logs_warning() {
        let mut engine = SkillTriggerEngine::new();
        // Invalid regex: unclosed bracket — should log warning and not panic
        engine.register_skill(
            r"[invalid",
            skill_ref("bad", "Bad Regex", "bad", 0.5),
        );
        assert_eq!(engine.triggers.len(), 5); // only the 5 defaults
    }

    #[test]
    fn test_case_insensitive_matching() {
        let engine = SkillTriggerEngine::new();
        let desc = "WRITE the document";
        let results = engine.match_skills(desc, &[]);
        assert!(results.iter().any(|s| s.id == "write"));
    }

    #[test]
    fn test_tag_fallback_case_insensitive() {
        let engine = SkillTriggerEngine::new();
        let tags: Vec<String> = vec!["CODE".into()];
        let results = engine.match_skills("irrelevant", &tags);
        assert!(results.iter().any(|s| s.id == "exec"));
    }
}
