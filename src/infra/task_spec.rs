//! TaskSpec parser — parses YAML frontmatter + markdown body files.
//!
//! File format:
//! ```text
//! ---
//! <YAML frontmatter>
//! ---
//! <Markdown body>
//! ```
//!
//! The YAML frontmatter is deserialized into a [`TaskSpec`] and the
//! remainder of the file populates the `description` field.

use std::path::Path;

use crate::infra::error::TaijiError;
use crate::types::task_spec::TaskSpec;

/// Parse a string containing YAML frontmatter (between `---` markers)
/// followed by a markdown body into a [`TaskSpec`].
///
/// # Errors
///
/// Returns [`TaijiError::Config`] when the frontmatter is missing,
/// malformed, or cannot be deserialised into a [`TaskSpec`].
pub fn parse_task_spec(content: &str) -> Result<TaskSpec, TaijiError> {
    let content = content.trim();

    // Must start with "---".
    if !content.starts_with("---") {
        return Err(TaijiError::Config {
            context: "task spec must start with YAML frontmatter delimited by '---'".into(),
        });
    }

    // Find the closing "---".
    let body_start = if let Some(end) = content[3..].find("\n---") {
        end + 3 // index within the original string
    } else {
        return Err(TaijiError::Config {
            context: "task spec frontmatter missing closing '---' delimiter".into(),
        });
    };

    let yaml_part = &content[3..body_start].trim();
    let md_part = content[body_start + 4..].trim(); // skip "\n---"

    if yaml_part.is_empty() {
        return Err(TaijiError::Config {
            context: "task spec frontmatter is empty".into(),
        });
    }

    let mut spec: TaskSpec = serde_yaml::from_str(yaml_part).map_err(|e| {
        TaijiError::Config {
            context: format!("failed to parse YAML frontmatter: {e}"),
        }
    })?;

    // Populate description from the markdown body if not already set.
    if spec.description.is_empty() && !md_part.is_empty() {
        spec.description = md_part.to_string();
    }

    Ok(spec)
}

/// Read a file at `path` and parse its contents as a task spec.
///
/// # Errors
///
/// Propagates IO errors as [`TaijiError::IO`] and parse errors as
/// [`TaijiError::Config`].
pub fn parse_task_spec_file(path: &Path) -> Result<TaskSpec, TaijiError> {
    let content = std::fs::read_to_string(path)?;
    parse_task_spec(&content)
}

/// Validate that a [`TaskSpec`] has all required fields populated.
///
/// Currently checks:
/// - `id` must not be empty
/// - `title` must not be empty
///
/// Returns `Ok(())` on success, or `Err` with a list of human-readable
/// validation error messages.
pub fn validate_task_spec(spec: &TaskSpec) -> Result<(), Vec<String>> {
    let mut errors: Vec<String> = Vec::new();

    if spec.id.trim().is_empty() {
        errors.push("'id' is required and must not be empty".to_string());
    }
    if spec.title.trim().is_empty() {
        errors.push("'title' is required and must not be empty".to_string());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_task_spec() {
        let content = r#"---
id: "test-001"
title: "Test Task"
description: "This is the task description body."
verification_spec: "verify something"
task_type_tags:
  - "refactor"
milestones:
  - name: "Phase 1"
    description: "First phase"
    verification: "check phase 1"
context: {"key": "value"}
---
This is the task description body.
"#;
        let spec = parse_task_spec(content).expect("should parse");
        assert_eq!(spec.id, "test-001");
        assert_eq!(spec.title, "Test Task");
        assert_eq!(spec.verification_spec, "verify something");
        assert!(spec.task_type_tags.contains(&"refactor".to_string()));
        assert_eq!(spec.milestones.len(), 1);
        assert_eq!(spec.milestones[0].name, "Phase 1");
        assert_eq!(spec.context["key"], "value");
        assert_eq!(spec.description, "This is the task description body.");
    }

    #[test]
    fn parse_no_frontmatter_fails() {
        let content = "just plain text without frontmatter";
        let result = parse_task_spec(content);
        assert!(result.is_err());
    }

    #[test]
    fn parse_unclosed_frontmatter_fails() {
        let content = "---\nid: test\n";
        let result = parse_task_spec(content);
        assert!(result.is_err());
    }

    #[test]
    fn parse_empty_frontmatter_fails() {
        let content = "---\n---\nbody text";
        let result = parse_task_spec(content);
        assert!(result.is_err());
    }

    #[test]
    fn parse_invalid_yaml_fails() {
        let content = "---\nid: [invalid yaml\n---\nbody";
        let result = parse_task_spec(content);
        assert!(result.is_err());
    }

    #[test]
    fn parse_body_only_no_description_override() {
        // When description is already set in YAML, the body does not override it.
        let content = "---\nid: t1\ntitle: T1\ndescription: explicit\nverification_spec: v\n---\nbody text";
        let spec = parse_task_spec(content).expect("should parse");
        assert_eq!(spec.description, "explicit");
    }

    #[test]
    fn validate_ok() {
        let spec = TaskSpec {
            id: "x".into(),
            title: "X".into(),
            description: "".into(),
            verification_spec: "".into(),
            task_type_tags: vec![],
            milestones: vec![],
            context: serde_json::Value::Null,
        };
        assert!(validate_task_spec(&spec).is_ok());
    }

    #[test]
    fn validate_fails_on_empty_id() {
        let spec = TaskSpec {
            id: "".into(),
            title: "X".into(),
            description: "".into(),
            verification_spec: "".into(),
            task_type_tags: vec![],
            milestones: vec![],
            context: serde_json::Value::Null,
        };
        let err = validate_task_spec(&spec).unwrap_err();
        assert!(err.iter().any(|m| m.contains("id")));
    }

    #[test]
    fn validate_fails_on_empty_title() {
        let spec = TaskSpec {
            id: "x".into(),
            title: "".into(),
            description: "".into(),
            verification_spec: "".into(),
            task_type_tags: vec![],
            milestones: vec![],
            context: serde_json::Value::Null,
        };
        let err = validate_task_spec(&spec).unwrap_err();
        assert!(err.iter().any(|m| m.contains("title")));
    }

    #[test]
    fn validate_fails_on_both_empty() {
        let spec = TaskSpec {
            id: "".into(),
            title: "".into(),
            description: "".into(),
            verification_spec: "".into(),
            task_type_tags: vec![],
            milestones: vec![],
            context: serde_json::Value::Null,
        };
        let err = validate_task_spec(&spec).unwrap_err();
        assert_eq!(err.len(), 2);
    }
}
