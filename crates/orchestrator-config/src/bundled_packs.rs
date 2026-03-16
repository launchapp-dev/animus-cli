use std::path::PathBuf;

use anyhow::Result;

use crate::pack_config::{load_pack_manifest, LoadedPackManifest};

const BUNDLED_PACK_IDS: [&str; 3] = ["ao.review", "ao.task", "ao.requirement"];

pub(crate) fn bundled_packs_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config").join("bundled-packs")
}

pub(crate) fn bundled_pack_root(pack_id: &str) -> PathBuf {
    bundled_packs_root().join(pack_id)
}

pub(crate) fn discover_bundled_pack_manifests() -> Result<Vec<LoadedPackManifest>> {
    let mut manifests = Vec::with_capacity(BUNDLED_PACK_IDS.len());
    for pack_id in BUNDLED_PACK_IDS {
        manifests.push(load_pack_manifest(&bundled_pack_root(pack_id))?);
    }
    Ok(manifests)
}
