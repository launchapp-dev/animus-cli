use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

pub fn validate_workspace(cwd: &str, project_root: &str) -> Result<()> {
    let cwd_path = PathBuf::from(cwd)
        .canonicalize()
        .context("Invalid cwd path")?;

    let root_path = PathBuf::from(project_root)
        .canonicalize()
        .context("Invalid project_root path")?;

    let inside_project_root = cwd_path.starts_with(&root_path);
    let inside_managed_worktree = is_managed_worktree_for_project(&cwd_path, &root_path);
    if !inside_project_root && !inside_managed_worktree {
        bail!(
            "Security violation: cwd '{}' is not within project_root '{}'",
            cwd_path.display(),
            root_path.display()
        );
    }

    Ok(())
}

fn is_managed_worktree_for_project(candidate_cwd: &Path, project_root: &Path) -> bool {
    let mut cursor = candidate_cwd.parent();
    while let Some(path) = cursor {
        if path.file_name().and_then(|value| value.to_str()) == Some("worktrees") {
            let Some(repo_ao_root) = path.parent() else {
                return false;
            };
            let marker_path = repo_ao_root.join(".project-root");
            let Ok(marker_content) = std::fs::read_to_string(marker_path) else {
                return false;
            };
            let recorded_root = marker_content.trim();
            if recorded_root.is_empty() {
                return false;
            }
            let Ok(recorded_canonical) = Path::new(recorded_root).canonicalize() else {
                return false;
            };
            return recorded_canonical == project_root;
        }
        cursor = path.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_workspace_valid() {
        let temp = std::env::temp_dir();
        let result = validate_workspace(temp.to_str().unwrap(), temp.to_str().unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_workspace_invalid() {
        let result = validate_workspace("/tmp", "/home");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_workspace_accepts_managed_worktree() {
        let root = std::env::temp_dir().join(format!(
            "ao-workspace-guard-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).expect("temp root should be created");

        let project = root.join("project");
        std::fs::create_dir_all(&project).expect("project dir should be created");
        let project_canonical = project
            .canonicalize()
            .expect("project path should canonicalize");

        let worktree = root
            .join(".ao")
            .join("scope-abc123")
            .join("worktrees")
            .join("task-task-011");
        std::fs::create_dir_all(&worktree).expect("managed worktree should be created");
        std::fs::write(
            root.join(".ao").join("scope-abc123").join(".project-root"),
            format!("{}\n", project_canonical.to_string_lossy()),
        )
        .expect("project marker should be written");

        let result = validate_workspace(
            worktree.to_string_lossy().as_ref(),
            project.to_string_lossy().as_ref(),
        );
        assert!(result.is_ok());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn accepts_subdirectory_within_project_root() {
        let root = std::env::temp_dir().join(format!(
            "ao-wg-subdir-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|v| v.as_nanos())
                .unwrap_or_default()
        ));
        let subdir = root.join("src").join("deep").join("nested");
        std::fs::create_dir_all(&subdir).expect("subdir should be created");

        let result = validate_workspace(
            subdir.to_string_lossy().as_ref(),
            root.to_string_lossy().as_ref(),
        );
        assert!(result.is_ok());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_sibling_directory_outside_project_root() {
        let parent = std::env::temp_dir().join(format!(
            "ao-wg-sibling-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|v| v.as_nanos())
                .unwrap_or_default()
        ));
        let project = parent.join("project");
        let sibling = parent.join("other-repo");
        std::fs::create_dir_all(&project).expect("project should be created");
        std::fs::create_dir_all(&sibling).expect("sibling should be created");

        let result = validate_workspace(
            sibling.to_string_lossy().as_ref(),
            project.to_string_lossy().as_ref(),
        );
        assert!(result.is_err());

        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("Security violation"),
            "error should mention security violation, got: {}",
            err_msg
        );

        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn rejects_nonexistent_cwd_path() {
        let result = validate_workspace(
            "/tmp/ao-nonexistent-path-xyz-999",
            std::env::temp_dir().to_str().unwrap(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn rejects_nonexistent_project_root() {
        let result = validate_workspace(
            std::env::temp_dir().to_str().unwrap(),
            "/tmp/ao-nonexistent-root-xyz-999",
        );
        assert!(result.is_err());
    }

    #[test]
    fn rejects_worktree_with_wrong_project_root_marker() {
        let root = std::env::temp_dir().join(format!(
            "ao-wg-wrong-marker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|v| v.as_nanos())
                .unwrap_or_default()
        ));

        let project = root.join("project-a");
        let other_project = root.join("project-b");
        std::fs::create_dir_all(&project).expect("project should be created");
        std::fs::create_dir_all(&other_project).expect("other project should be created");

        let worktree = root
            .join(".ao")
            .join("scope-abc")
            .join("worktrees")
            .join("task-1");
        std::fs::create_dir_all(&worktree).expect("worktree should be created");

        let other_canonical = other_project
            .canonicalize()
            .expect("path should canonicalize");
        std::fs::write(
            root.join(".ao").join("scope-abc").join(".project-root"),
            format!("{}\n", other_canonical.to_string_lossy()),
        )
        .expect("marker should be written");

        let result = validate_workspace(
            worktree.to_string_lossy().as_ref(),
            project.to_string_lossy().as_ref(),
        );
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&root);
    }
}
