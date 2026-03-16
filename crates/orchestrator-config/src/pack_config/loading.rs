use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::types::{PackManifest, PACK_MANIFEST_FILE_NAME};
use super::validation::{validate_pack_manifest, validate_pack_manifest_assets};

#[derive(Debug, Clone)]
pub struct LoadedPackManifest {
    pub manifest: PackManifest,
    pub pack_root: PathBuf,
    pub manifest_path: PathBuf,
}

pub fn pack_manifest_path(pack_root: &Path) -> PathBuf {
    pack_root.join(PACK_MANIFEST_FILE_NAME)
}

pub fn parse_pack_manifest(raw_toml: &str) -> Result<PackManifest> {
    let manifest: PackManifest = toml::from_str(raw_toml).context("failed to parse pack manifest TOML")?;
    validate_pack_manifest(&manifest)?;
    Ok(manifest)
}

pub fn load_pack_manifest(pack_root: &Path) -> Result<LoadedPackManifest> {
    let manifest_path = pack_manifest_path(pack_root);
    load_pack_manifest_from_file(&manifest_path)
}

pub fn load_pack_manifest_from_file(manifest_path: &Path) -> Result<LoadedPackManifest> {
    let raw_toml = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read pack manifest at {}", manifest_path.display()))?;
    let manifest = parse_pack_manifest(&raw_toml)?;

    let pack_root = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .with_context(|| format!("pack manifest path '{}' has no parent directory", manifest_path.display()))?;
    validate_pack_manifest_assets(&pack_root, &manifest)?;

    Ok(LoadedPackManifest { manifest, pack_root, manifest_path: manifest_path.to_path_buf() })
}
