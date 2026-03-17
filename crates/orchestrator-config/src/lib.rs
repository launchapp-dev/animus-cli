pub mod agent_runtime_config;
mod bundled_packs;
mod json;
pub mod pack_config;
pub mod pack_marketplace;
pub mod pack_registry;
pub mod pack_selection;
pub mod skill_definition;
pub mod skill_resolution;
pub mod skill_scoping;
#[cfg(test)]
pub(crate) mod test_support;
pub mod workflow_config;

pub const DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE: usize = 3;

pub mod domain_state {
    pub use crate::json::write_json_pretty;
}

pub mod workflow {
    pub use crate::DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE;
}

pub mod types {
    pub use protocol::orchestrator::{PhaseEvidenceKind, WorkflowDecisionRisk};
}

pub use agent_runtime_config::*;
pub(crate) use bundled_packs::discover_bundled_pack_manifests;
pub use pack_config::{
    activate_pack_mcp_overlay, apply_pack_mcp_overlay, check_pack_runtime_requirements,
    ensure_pack_runtime_requirements, load_pack_manifest, load_pack_manifest_from_file, load_pack_mcp_overlay,
    pack_manifest_path, parse_pack_manifest, validate_pack_manifest, validate_pack_manifest_assets,
    ExternalRuntimeKind, LoadedPackManifest, PackCompatibility, PackDependency, PackKind, PackManifest, PackMcp,
    PackMcpOverlay, PackNativeModule, PackOwnership, PackOwnershipMode, PackPermissions, PackRuntime, PackRuntimeCheck,
    PackRuntimeCheckStatus, PackRuntimeReport, PackRuntimeRequirement, PackSchedules, PackSecrets, PackSubjects,
    PackWorkflows, PACK_MANIFEST_FILE_NAME, PACK_MANIFEST_SCHEMA_ID,
};
pub use pack_marketplace::{
    add_marketplace_registry, clone_marketplace_pack, load_marketplace_state, remove_marketplace_registry,
    search_marketplace_packs, sync_all_registries, sync_registry, MarketplaceEntry, MarketplaceSearchResult,
    MarketplaceState,
};
pub use pack_registry::{
    ensure_pack_execution_requirements, load_pack_agent_runtime_overlay, load_pack_inventory,
    load_pack_workflow_overlay, machine_installed_packs_dir, project_pack_overrides_dir,
    resolve_active_pack_for_workflow_ref, resolve_pack_registry, validate_active_pack_configuration, PackInventory,
    PackInventoryEntry, PackRegistrySource, ResolvedPackRegistry, ResolvedPackRegistryEntry, BUNDLED_BUILTIN_PACK_ID,
    BUNDLED_BUILTIN_PACK_VERSION, MACHINE_PACKS_DIR_NAME, PROJECT_PACKS_DIR_NAME,
};
pub use pack_selection::{
    load_pack_selection_state, pack_selection_path, save_pack_selection_state, PackSelectionEntry, PackSelectionSource,
    PackSelectionState, PACK_SELECTION_FILE_NAME, PACK_SELECTION_SCHEMA_ID,
};
pub use skill_definition::*;
pub use workflow_config::*;
