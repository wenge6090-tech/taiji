//! Human-readable task ID generation (V26.6).
//!
//! Task IDs moved from raw UUIDs to `{slug}-{YYYYMMDD-HHMMSS}` (e.g.
//! `分析源码架构-20260807-061530`) so that task directories, `taiji list`,
//! `--resume <id>` and the frontend tree show what the task is about and when
//! it started.
//!
//! - `slug`: first [`SLUG_MAX_CHARS`] chars of the description, path-sanitized
//!   (non-alphanumeric → `-`, runs collapsed, leading/trailing dashes trimmed,
//!   empty → `task`).
//! - timestamp: local time, second precision.
//! - Uniqueness is NOT guaranteed by the generator alone — callers use
//!   [`ensure_unique`] (root tasks check `tasks/<id>` dir existence; child
//!   tasks append `-{index}` which is unique within a parent).

use chrono::Local;

/// Max number of characters taken from the description for the slug.
const SLUG_MAX_CHARS: usize = 24;

/// Build a path-safe slug from a task description.
///
/// Keeps alphanumerics (Unicode-aware, so CJK survives) plus `_`/`-`; any
/// other character (spaces, `/`, `\`, `:`, `.`, newlines, …) becomes `-`.
/// Consecutive dashes are collapsed and leading/trailing dashes trimmed so
/// the result is safe as a single path segment and never looks like `..`.
fn slugify(description: &str) -> String {
    let mut slug = String::with_capacity(SLUG_MAX_CHARS);
    let mut pending_dash = false;
    for c in description.chars().take(SLUG_MAX_CHARS) {
        if c.is_alphanumeric() || c == '_' {
            // Flush a single separator dash before the next token, but never
            // at the very start (leading dashes trimmed).
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(c);
        } else {
            // Spaces, `/`, `\`, `:`, `.`, `-`, newlines … all become one dash.
            pending_dash = true;
        }
    }
    // Trailing dashes are dropped implicitly (pending_dash never flushed).
    if slug.is_empty() {
        slug.push_str("task");
    }
    slug
}

/// Generate a task ID: `{slug}-{YYYYMMDD-HHMMSS}` (local time).
///
/// The result is NOT guaranteed unique on its own — pair with
/// [`ensure_unique`] or append a parent-scoped index.
pub fn generate_task_id(description: &str) -> String {
    format!(
        "{}-{}",
        slugify(description),
        Local::now().format("%Y%m%d-%H%M%S")
    )
}

/// Append `-2`, `-3`, … to `id` until `exists(&candidate)` returns false.
///
/// Used by the runner for root tasks: the description timestamp has second
/// precision, so two tasks started in the same second with the same slug
/// would collide on `.taiji/tasks/<id>` — this resolves it deterministically.
pub fn ensure_unique(id: String, exists: impl Fn(&str) -> bool) -> String {
    if !exists(&id) {
        return id;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{id}-{n}");
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_keeps_cjk_and_alphanumerics() {
        assert_eq!(slugify("分析源码架构"), "分析源码架构");
        assert_eq!(slugify("analysis 2026 report"), "analysis-2026-report");
    }

    #[test]
    fn slugify_sanitizes_path_dangerous_chars() {
        // No `/`, `\`, `:`, `.`, spaces survive — single path segment, no `..`.
        let s = slugify("src/../lib: 修复 \"bug\" * v1.0?");
        assert!(!s.contains('/'));
        assert!(!s.contains('\\'));
        assert!(!s.contains(':'));
        assert!(!s.contains(".."));
        assert!(!s.contains('"'));
        assert!(!s.contains('*'));
        assert!(!s.contains('?'));
        assert!(!s.contains(' '));
        assert!(!s.contains('.'));
    }

    #[test]
    fn slugify_collapses_dashes_and_trims() {
        assert_eq!(slugify("  a   b  "), "a-b");
        assert_eq!(slugify("---foo---"), "foo");
        assert_eq!(slugify("..."), "task"); // nothing alphanumeric left
        assert_eq!(slugify(""), "task");
    }

    #[test]
    fn slugify_truncates_to_slug_max_chars() {
        let long = "x".repeat(SLUG_MAX_CHARS + 20);
        let s = slugify(&long);
        assert_eq!(s.chars().count(), SLUG_MAX_CHARS);
    }

    #[test]
    fn generate_task_id_has_expected_shape() {
        let id = generate_task_id("分析源码架构");
        // `{slug}-{YYYYMMDD-HHMMSS}` — the timestamp is the trailing 15 ASCII
        // chars (`%Y%m%d-%H%M%S` = 8+1+6).
        let ts = &id[id.len() - 15..];
        assert_eq!(ts.len(), 15, "timestamp must be %Y%m%d-%H%M%S");
        assert!(ts.chars().all(|c| c.is_ascii_digit() || c == '-'));
        assert_eq!(&id[..id.len() - 16], "分析源码架构");
        // Timestamp parses as a plausible date.
        let date = &ts[..8];
        let year: u32 = date[..4].parse().unwrap();
        assert!((2020..2100).contains(&year));
    }

    #[test]
    fn ensure_unique_appends_incrementing_suffix() {
        let taken = |candidate: &str| {
            matches!(
                candidate,
                "task-20260807-061530"
                    | "task-20260807-061530-2"
                    | "task-20260807-061530-3"
            )
        };
        let id = ensure_unique("task-20260807-061530".to_string(), taken);
        assert_eq!(id, "task-20260807-061530-4");
    }

    #[test]
    fn ensure_unique_passes_through_when_free() {
        let id = ensure_unique("task-20260807-061530".to_string(), |_| false);
        assert_eq!(id, "task-20260807-061530");
    }
}
