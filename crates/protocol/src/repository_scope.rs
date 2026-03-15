use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub fn scoped_state_root(project_root: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".ao").join(repository_scope_for_path(project_root)))
}

pub fn sanitize_identifier(value: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut trailing_separator = false;

    for ch in value.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => {
                out.push(ch.to_ascii_lowercase());
                trailing_separator = false;
            }
            ' ' | '_' | '-' => {
                if !out.is_empty() && !trailing_separator {
                    out.push('-');
                    trailing_separator = true;
                }
            }
            _ => {}
        }
    }

    if trailing_separator {
        out.pop();
    }

    if out.is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

pub fn repository_scope_for_path(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let canonical_display = canonical.to_string_lossy();
    let repo_name = canonical
        .file_name()
        .and_then(|value| value.to_str())
        .map(|s| sanitize_identifier(s, "repo"))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "repo".to_string());

    let mut hasher = Sha256::new();
    hasher.update(canonical_display.as_bytes());
    let digest = hasher.finalize();
    let suffix = format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
    );
    format!("{repo_name}-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn sanitize_identifier_normalizes_expected_shapes() {
        assert_eq!(sanitize_identifier("Repo Name", "repo"), "repo-name");
        assert_eq!(sanitize_identifier("___", "repo"), "repo");
        assert_eq!(sanitize_identifier("A__B--C", "repo"), "a-b-c");
        assert_eq!(sanitize_identifier("  __My Repo!! -- 2026__  ", "repo"), "my-repo-2026");
        assert_eq!(sanitize_identifier("日本語", "repo"), "repo");
        assert_eq!(sanitize_identifier("日本語", "task"), "task");
    }

    #[test]
    fn repository_scope_for_path_uses_canonical_basename() {
        let root = tempfile::tempdir().expect("tempdir");
        let canonical = root.path().join("Canonical Repo");
        std::fs::create_dir_all(&canonical).expect("create canonical path");
        let alias = root.path().join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&canonical, &alias).expect("create symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&canonical, &alias).expect("create symlink");

        let scope = repository_scope_for_path(&alias);
        assert!(scope.starts_with("canonical-repo-"));
    }

    #[test]
    fn repository_scope_for_path_emits_slug_and_12_hex_suffix() {
        let temp = tempfile::tempdir().expect("tempdir");
        let scope = repository_scope_for_path(temp.path());
        let (slug, suffix) = scope.rsplit_once('-').expect("scope should contain hyphen");
        assert!(!slug.is_empty());
        assert_eq!(suffix.len(), 12);
        assert!(suffix.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_eq!(suffix, suffix.to_ascii_lowercase());
    }

    #[test]
    fn repository_scope_for_path_uses_raw_path_when_canonicalize_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("Missing Repo 2026");

        let scope = repository_scope_for_path(&missing);
        assert!(scope.starts_with("missing-repo-2026-"));
    }

    proptest! {
        #[test]
        fn sanitize_identifier_output_contains_only_valid_chars(input in "\\PC*") {
            let result = sanitize_identifier(&input, "fallback");
            prop_assert!(result.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-'));
            prop_assert!(!result.is_empty());
            prop_assert!(!result.starts_with('-'));
            prop_assert!(!result.ends_with('-'));
        }

        #[test]
        fn sanitize_identifier_is_idempotent(input in "\\PC*") {
            let once = sanitize_identifier(&input, "fallback");
            let twice = sanitize_identifier(&once, "fallback");
            prop_assert_eq!(once, twice);
        }

        #[test]
        fn repository_scope_for_path_never_panics(input in "\\PC{1,200}") {
            let path = std::path::Path::new(&input);
            let _scope = repository_scope_for_path(path);
        }
    }
}
