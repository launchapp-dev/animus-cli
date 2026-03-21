use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use include_dir::{include_dir, Dir};

use crate::pack_config::{load_pack_manifest, LoadedPackManifest};

const BUNDLED_PACK_IDS: [&str; 3] = ["ao.review", "ao.task", "ao.requirement"];

static EMBEDDED_PACKS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/config/bundled-packs");

static MATERIALIZED_ROOT: OnceLock<PathBuf> = OnceLock::new();

fn materialize_bundled_packs() -> Result<&'static PathBuf> {
    if let Some(root) = MATERIALIZED_ROOT.get() {
        return Ok(root);
    }
    let dir = std::env::temp_dir().join(format!("ao-bundled-packs-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("failed to clean stale bundled packs at {}", dir.display()))?;
    }
    extract_dir(&EMBEDDED_PACKS, &dir)?;
    let _ = MATERIALIZED_ROOT.set(dir);
    Ok(MATERIALIZED_ROOT.get().expect("just set"))
}

fn extract_dir(dir: &Dir<'_>, target: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(target).with_context(|| format!("failed to create directory {}", target.display()))?;
    for file in dir.files() {
        let file_path = target.join(file.path().file_name().unwrap_or(file.path().as_os_str()));
        std::fs::write(&file_path, file.contents())
            .with_context(|| format!("failed to write {}", file_path.display()))?;
    }
    for subdir in dir.dirs() {
        let subdir_name = subdir.path().file_name().unwrap_or(subdir.path().as_os_str());
        extract_dir(subdir, &target.join(subdir_name))?;
    }
    Ok(())
}

pub(crate) fn bundled_packs_root() -> Result<PathBuf> {
    Ok(materialize_bundled_packs()?.clone())
}

pub(crate) fn bundled_pack_root(pack_id: &str) -> Result<PathBuf> {
    Ok(bundled_packs_root()?.join(pack_id))
}

pub(crate) fn discover_bundled_pack_manifests() -> Result<Vec<LoadedPackManifest>> {
    let mut manifests = Vec::with_capacity(BUNDLED_PACK_IDS.len());
    for pack_id in BUNDLED_PACK_IDS {
        manifests.push(load_pack_manifest(&bundled_pack_root(pack_id)?)?);
    }
    Ok(manifests)
}
