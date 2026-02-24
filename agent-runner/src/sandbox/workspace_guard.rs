use anyhow::{bail, Context, Result};
use std::path::PathBuf;

pub fn validate_workspace(cwd: &str, project_root: &str) -> Result<()> {
    let cwd_path = PathBuf::from(cwd)
        .canonicalize()
        .context("Invalid cwd path")?;

    let root_path = PathBuf::from(project_root)
        .canonicalize()
        .context("Invalid project_root path")?;

    if !cwd_path.starts_with(&root_path) {
        bail!(
            "Security violation: cwd '{}' is not within project_root '{}'",
            cwd_path.display(),
            root_path.display()
        );
    }

    Ok(())
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
}
