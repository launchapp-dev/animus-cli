pub mod loading;
pub mod mcp;
pub mod runtime;
pub mod types;
pub mod validation;

#[cfg(test)]
mod tests;

pub use loading::{
    load_pack_manifest, load_pack_manifest_from_file, pack_manifest_path, parse_pack_manifest, LoadedPackManifest,
};
pub use mcp::{apply_pack_mcp_overlay, load_pack_mcp_overlay, PackMcpOverlay};
pub use runtime::{
    activate_pack_mcp_overlay, check_pack_runtime_requirements, ensure_pack_runtime_requirements, PackRuntimeCheck,
    PackRuntimeCheckStatus, PackRuntimeReport,
};
pub use types::*;
pub use validation::{validate_pack_manifest, validate_pack_manifest_assets};
