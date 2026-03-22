use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use semver::{Version, VersionReq};

use crate::agent_runtime_config::{AgentRuntimeOverlay, CliToolConfig, PhaseExecutionDefinition};
use crate::bundled_packs::discover_bundled_pack_manifests;
use crate::pack_config::{load_pack_manifest, LoadedPackManifest};
use crate::pack_selection::{load_pack_selection_state, PackSelectionEntry, PackSelectionState};
use crate::workflow_config::{compile_yaml_sources_with_base, WorkflowConfig};

pub const PROJECT_PACKS_DIR_NAME: &str = "plugins";
pub const MACHINE_PACKS_DIR_NAME: &str = "packs";
pub const BUNDLED_BUILTIN_PACK_ID: &str = "builtin";
pub const BUNDLED_BUILTIN_PACK_VERSION: &str = "builtin";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackRegistrySource {
    Bundled,
    Installed,
    ProjectOverride,
}

impl PackRegistrySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::Installed => "installed",
            Self::ProjectOverride => "project_override",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedPackRegistryEntry {
    pub pack_id: String,
    pub version: String,
    pub source: PackRegistrySource,
    pub pack_root: Option<PathBuf>,
    pub manifest_path: Option<PathBuf>,
    loaded_manifest: Option<LoadedPackManifest>,
}

impl ResolvedPackRegistryEntry {
    fn bundled_builtin() -> Self {
        Self {
            pack_id: BUNDLED_BUILTIN_PACK_ID.to_string(),
            version: BUNDLED_BUILTIN_PACK_VERSION.to_string(),
            source: PackRegistrySource::Bundled,
            pack_root: None,
            manifest_path: None,
            loaded_manifest: None,
        }
    }

    fn from_manifest(source: PackRegistrySource, loaded_manifest: LoadedPackManifest) -> Self {
        Self {
            pack_id: loaded_manifest.manifest.id.clone(),
            version: loaded_manifest.manifest.version.clone(),
            source,
            pack_root: Some(loaded_manifest.pack_root.clone()),
            manifest_path: Some(loaded_manifest.manifest_path.clone()),
            loaded_manifest: Some(loaded_manifest),
        }
    }

    pub fn loaded_manifest(&self) -> Option<&LoadedPackManifest> {
        self.loaded_manifest.as_ref()
    }
}

#[derive(Debug, Clone)]
pub struct PackInventoryEntry {
    pub pack_id: String,
    pub version: String,
    pub source: PackRegistrySource,
    pub pack_root: Option<PathBuf>,
    pub manifest_path: Option<PathBuf>,
    pub active: bool,
    pub selection: Option<PackSelectionEntry>,
    loaded_manifest: Option<LoadedPackManifest>,
}

impl PackInventoryEntry {
    fn bundled_builtin(active: bool) -> Self {
        Self {
            pack_id: BUNDLED_BUILTIN_PACK_ID.to_string(),
            version: BUNDLED_BUILTIN_PACK_VERSION.to_string(),
            source: PackRegistrySource::Bundled,
            pack_root: None,
            manifest_path: None,
            active,
            selection: None,
            loaded_manifest: None,
        }
    }

    fn from_manifest(
        source: PackRegistrySource,
        loaded_manifest: LoadedPackManifest,
        active: bool,
        selection: Option<PackSelectionEntry>,
    ) -> Self {
        Self {
            pack_id: loaded_manifest.manifest.id.clone(),
            version: loaded_manifest.manifest.version.clone(),
            source,
            pack_root: Some(loaded_manifest.pack_root.clone()),
            manifest_path: Some(loaded_manifest.manifest_path.clone()),
            active,
            selection,
            loaded_manifest: Some(loaded_manifest),
        }
    }

    pub fn loaded_manifest(&self) -> Option<&LoadedPackManifest> {
        self.loaded_manifest.as_ref()
    }
}

#[derive(Debug, Clone, Default)]
pub struct PackInventory {
    pub entries: Vec<PackInventoryEntry>,
}

impl PackInventory {
    pub fn resolve(
        &self,
        pack_id: &str,
        version: Option<&str>,
        source: Option<PackRegistrySource>,
    ) -> Option<&PackInventoryEntry> {
        self.entries.iter().find(|entry| {
            entry.pack_id.eq_ignore_ascii_case(pack_id)
                && version.map(|candidate| entry.version == candidate).unwrap_or(true)
                && source.map(|candidate| entry.source == candidate).unwrap_or(true)
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedPackRegistry {
    pub entries: Vec<ResolvedPackRegistryEntry>,
}

impl ResolvedPackRegistry {
    pub fn has_external_packs(&self) -> bool {
        self.entries.iter().any(|entry| entry.source != PackRegistrySource::Bundled)
    }

    pub fn has_pack_overlays(&self) -> bool {
        self.entries.iter().any(|entry| entry.loaded_manifest().is_some())
    }

    pub fn resolve(&self, pack_id: &str) -> Option<&ResolvedPackRegistryEntry> {
        self.entries.iter().find(|entry| entry.pack_id.eq_ignore_ascii_case(pack_id))
    }

    pub fn entries_for_source(
        &self,
        source: PackRegistrySource,
    ) -> impl Iterator<Item = &ResolvedPackRegistryEntry> + '_ {
        self.entries.iter().filter(move |entry| entry.source == source)
    }
}

pub fn project_pack_overrides_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ao").join(PROJECT_PACKS_DIR_NAME)
}

pub fn machine_installed_packs_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".ao").join(MACHINE_PACKS_DIR_NAME)
}

struct PackRegistryInputs {
    selection_state: PackSelectionState,
    bundled_packs: Vec<LoadedPackManifest>,
    installed_versions: Vec<LoadedPackManifest>,
    project_overrides: Vec<LoadedPackManifest>,
}

fn discover_pack_registry_inputs(project_root: &Path) -> Result<PackRegistryInputs> {
    Ok(PackRegistryInputs {
        selection_state: load_pack_selection_state(project_root)?,
        bundled_packs: discover_bundled_pack_manifests()?,
        installed_versions: discover_installed_pack_versions()?,
        project_overrides: discover_project_override_packs(project_root)?,
    })
}

fn resolve_pack_registry_from_inputs(inputs: &PackRegistryInputs) -> Result<ResolvedPackRegistry> {
    let bundled_by_id = map_bundled_packs_by_id(&inputs.bundled_packs)?;
    let installed_by_id = group_installed_packs_by_id(&inputs.installed_versions)?;
    let overrides_by_id = map_project_overrides_by_id(&inputs.project_overrides)?;

    let mut entries = vec![ResolvedPackRegistryEntry::bundled_builtin()];
    let mut pack_ids = installed_by_id
        .keys()
        .chain(bundled_by_id.keys())
        .chain(overrides_by_id.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    pack_ids.remove(BUNDLED_BUILTIN_PACK_ID);

    for pack_id in pack_ids {
        let selection = inputs.selection_state.selection_for(&pack_id);
        if selection.is_some_and(|entry| !entry.enabled) {
            continue;
        }

        let selected = select_resolved_pack(
            &pack_id,
            selection,
            bundled_by_id.get(&pack_id).copied(),
            installed_by_id.get(&pack_id).map(Vec::as_slice),
            overrides_by_id.get(&pack_id).copied(),
        )?;
        if let Some(entry) = selected {
            entries.push(entry);
        }
    }

    Ok(ResolvedPackRegistry { entries })
}

pub fn resolve_pack_registry(project_root: &Path) -> Result<ResolvedPackRegistry> {
    resolve_pack_registry_from_inputs(&discover_pack_registry_inputs(project_root)?)
}

pub fn load_pack_inventory(project_root: &Path) -> Result<PackInventory> {
    let inputs = discover_pack_registry_inputs(project_root)?;
    let resolved = resolve_pack_registry_from_inputs(&inputs)?;
    let PackRegistryInputs { selection_state, bundled_packs, installed_versions, project_overrides } = inputs;

    let mut entries = vec![PackInventoryEntry::bundled_builtin(resolved.resolve(BUNDLED_BUILTIN_PACK_ID).is_some())];

    let mut bundled_packs = bundled_packs;
    bundled_packs.sort_by(|left, right| {
        left.manifest
            .id
            .cmp(&right.manifest.id)
            .then_with(|| compare_pack_versions_desc(&left.manifest.version, &right.manifest.version))
            .then_with(|| left.pack_root.cmp(&right.pack_root))
    });
    for pack in bundled_packs {
        let active = resolved_entry_matches_manifest(&resolved, PackRegistrySource::Bundled, &pack);
        let selection = selection_state.selection_for(&pack.manifest.id).cloned();
        entries.push(PackInventoryEntry::from_manifest(PackRegistrySource::Bundled, pack, active, selection));
    }

    let mut installed_versions = installed_versions;
    installed_versions.sort_by(|left, right| {
        left.manifest
            .id
            .cmp(&right.manifest.id)
            .then_with(|| compare_pack_versions_desc(&left.manifest.version, &right.manifest.version))
            .then_with(|| left.pack_root.cmp(&right.pack_root))
    });
    for pack in installed_versions {
        let active = resolved_entry_matches_manifest(&resolved, PackRegistrySource::Installed, &pack);
        let selection = selection_state.selection_for(&pack.manifest.id).cloned();
        entries.push(PackInventoryEntry::from_manifest(PackRegistrySource::Installed, pack, active, selection));
    }

    let mut project_overrides = project_overrides;
    project_overrides.sort_by(|left, right| {
        left.manifest.id.cmp(&right.manifest.id).then_with(|| left.pack_root.cmp(&right.pack_root))
    });
    for pack in project_overrides {
        let active = resolved_entry_matches_manifest(&resolved, PackRegistrySource::ProjectOverride, &pack);
        let selection = selection_state.selection_for(&pack.manifest.id).cloned();
        entries.push(PackInventoryEntry::from_manifest(PackRegistrySource::ProjectOverride, pack, active, selection));
    }

    Ok(PackInventory { entries })
}

pub fn load_pack_workflow_overlay(pack: &LoadedPackManifest, base: &WorkflowConfig) -> Result<Option<WorkflowConfig>> {
    let yaml_sources = collect_pack_workflow_yaml_sources(pack)?;
    if yaml_sources.is_empty() {
        return Ok(None);
    }

    let mut overlay = compile_yaml_sources_with_base(base, &yaml_sources)?;
    if let Some(overlay) = overlay.as_mut() {
        resolve_pack_workflow_assets(pack, overlay)?;
    }
    Ok(overlay)
}

pub fn load_pack_agent_runtime_overlay(pack: &LoadedPackManifest) -> Result<Option<AgentRuntimeOverlay>> {
    let Some(overlay_path) = pack.manifest.runtime.agent_overlay.as_deref() else {
        return Ok(None);
    };

    let path = pack.pack_root.join(overlay_path);
    let raw = fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut overlay: AgentRuntimeOverlay =
        serde_yaml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))?;
    resolve_pack_agent_runtime_assets(pack, &mut overlay)?;
    Ok(Some(overlay))
}

fn discover_project_override_packs(project_root: &Path) -> Result<Vec<LoadedPackManifest>> {
    let overrides_dir = project_pack_overrides_dir(project_root);
    if !overrides_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut pack_roots = read_child_directories(&overrides_dir)?;
    pack_roots.sort();

    let mut loaded = Vec::new();
    let mut seen_ids = BTreeMap::<String, PathBuf>::new();
    for pack_root in pack_roots {
        if !pack_root.join(crate::PACK_MANIFEST_FILE_NAME).exists() {
            continue;
        }

        let pack = load_pack_manifest(&pack_root)
            .with_context(|| format!("failed to load project override pack at {}", pack_root.display()))?;
        let id_key = pack.manifest.id.to_ascii_lowercase();
        if let Some(previous) = seen_ids.insert(id_key.clone(), pack_root.clone()) {
            return Err(anyhow!(
                "duplicate project override pack '{}' in '{}' and '{}'",
                pack.manifest.id,
                previous.display(),
                pack_root.display()
            ));
        }
        loaded.push(pack);
    }

    loaded.sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
    Ok(loaded)
}

fn discover_installed_pack_versions() -> Result<Vec<LoadedPackManifest>> {
    let installed_root = machine_installed_packs_dir();
    if !installed_root.is_dir() {
        return Ok(Vec::new());
    }

    let mut discovered = Vec::new();
    let mut pack_roots = read_child_directories(&installed_root)?;
    pack_roots.sort();

    for pack_dir in pack_roots {
        let mut version_dirs = read_child_directories(&pack_dir)?;
        version_dirs.sort();
        for version_dir in version_dirs {
            if !version_dir.join(crate::PACK_MANIFEST_FILE_NAME).exists() {
                continue;
            }

            let pack = load_pack_manifest(&version_dir)
                .with_context(|| format!("failed to load installed pack at {}", version_dir.display()))?;
            Version::parse(&pack.manifest.version)
                .with_context(|| format!("invalid installed pack version '{}'", pack.manifest.version))?;
            discovered.push(pack);
        }
    }

    Ok(discovered)
}

fn group_installed_packs_by_id(
    installed_versions: &[LoadedPackManifest],
) -> Result<BTreeMap<String, Vec<&LoadedPackManifest>>> {
    let mut grouped = BTreeMap::<String, Vec<&LoadedPackManifest>>::new();
    for pack in installed_versions {
        grouped.entry(pack.manifest.id.to_ascii_lowercase()).or_default().push(pack);
    }

    for versions in grouped.values_mut() {
        versions.sort_by(|left, right| {
            compare_pack_versions_desc(&left.manifest.version, &right.manifest.version)
                .then_with(|| left.pack_root.cmp(&right.pack_root))
        });

        for pack in versions.iter() {
            Version::parse(&pack.manifest.version)
                .with_context(|| format!("invalid installed pack version '{}'", pack.manifest.version))?;
        }
    }

    Ok(grouped)
}

fn map_project_overrides_by_id(
    project_overrides: &[LoadedPackManifest],
) -> Result<BTreeMap<String, &LoadedPackManifest>> {
    let mut mapped = BTreeMap::new();
    for pack in project_overrides {
        let key = pack.manifest.id.to_ascii_lowercase();
        if mapped.insert(key.clone(), pack).is_some() {
            return Err(anyhow!("duplicate project override pack id '{}'", key));
        }
    }
    Ok(mapped)
}

fn map_bundled_packs_by_id(bundled_packs: &[LoadedPackManifest]) -> Result<BTreeMap<String, &LoadedPackManifest>> {
    let mut mapped = BTreeMap::new();
    for pack in bundled_packs {
        let key = pack.manifest.id.to_ascii_lowercase();
        if mapped.insert(key.clone(), pack).is_some() {
            return Err(anyhow!("duplicate bundled pack id '{}'", key));
        }
    }
    Ok(mapped)
}

fn select_resolved_pack(
    pack_id: &str,
    selection: Option<&PackSelectionEntry>,
    bundled: Option<&LoadedPackManifest>,
    installed: Option<&[&LoadedPackManifest]>,
    project_override: Option<&LoadedPackManifest>,
) -> Result<Option<ResolvedPackRegistryEntry>> {
    let source_order =
        selection.and_then(|entry| entry.source).map(|source| vec![source.as_registry_source()]).unwrap_or_else(|| {
            vec![PackRegistrySource::ProjectOverride, PackRegistrySource::Installed, PackRegistrySource::Bundled]
        });

    for source in source_order {
        match source {
            PackRegistrySource::ProjectOverride => {
                let Some(pack) = project_override else {
                    continue;
                };
                if version_matches_selection(selection, &pack.manifest.version)? {
                    return Ok(Some(ResolvedPackRegistryEntry::from_manifest(
                        PackRegistrySource::ProjectOverride,
                        pack.clone(),
                    )));
                }
            }
            PackRegistrySource::Installed => {
                let Some(installed_versions) = installed else {
                    continue;
                };
                for &pack in installed_versions {
                    if version_matches_selection(selection, &pack.manifest.version)? {
                        return Ok(Some(ResolvedPackRegistryEntry::from_manifest(
                            PackRegistrySource::Installed,
                            pack.clone(),
                        )));
                    }
                }
            }
            PackRegistrySource::Bundled => {
                let Some(pack) = bundled else {
                    continue;
                };
                if version_matches_selection(selection, &pack.manifest.version)? {
                    return Ok(Some(ResolvedPackRegistryEntry::from_manifest(
                        PackRegistrySource::Bundled,
                        pack.clone(),
                    )));
                }
            }
        }
    }

    if let Some(selection) = selection {
        let mut requirement = format!("pack selection for '{}'", pack_id);
        if let Some(source) = selection.source {
            requirement.push_str(&format!(" source '{}'", source.as_registry_source().as_str()));
        }
        if let Some(version) = selection.version.as_deref() {
            requirement.push_str(&format!(" version '{}'", version.trim()));
        }
        return Err(anyhow!("{requirement} could not be satisfied"));
    }

    Ok(None)
}

fn version_matches_selection(selection: Option<&PackSelectionEntry>, actual_version: &str) -> Result<bool> {
    let Some(selection) = selection else {
        return Ok(true);
    };
    let Some(version_req) = selection.version.as_deref() else {
        return Ok(true);
    };

    let requirement = VersionReq::parse(version_req.trim())
        .with_context(|| format!("invalid selection version requirement '{}'", version_req.trim()))?;
    let actual =
        Version::parse(actual_version).with_context(|| format!("invalid pack version '{}'", actual_version))?;
    Ok(requirement.matches(&actual))
}

fn compare_pack_versions_desc(left: &str, right: &str) -> std::cmp::Ordering {
    match (Version::parse(left), Version::parse(right)) {
        (Ok(left), Ok(right)) => right.cmp(&left),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => right.cmp(left),
    }
}

fn resolved_entry_matches_manifest(
    resolved: &ResolvedPackRegistry,
    source: PackRegistrySource,
    pack: &LoadedPackManifest,
) -> bool {
    resolved.entries.iter().any(|entry| {
        entry.source == source
            && entry.pack_id.eq_ignore_ascii_case(&pack.manifest.id)
            && entry.version == pack.manifest.version
            && entry.pack_root.as_ref() == Some(&pack.pack_root)
    })
}

pub fn validate_active_pack_configuration(registry: &ResolvedPackRegistry) -> Result<()> {
    let mut active_by_id = BTreeMap::new();
    for entry in &registry.entries {
        active_by_id.insert(entry.pack_id.to_ascii_lowercase(), entry);
    }

    for entry in &registry.entries {
        let Some(pack) = entry.loaded_manifest() else {
            continue;
        };

        validate_pack_dependency_policy(pack, &active_by_id)?;
        validate_pack_permission_policy(pack)?;
        validate_pack_secret_policy(pack)?;
    }

    Ok(())
}

pub fn resolve_active_pack_for_workflow_ref<'a>(
    registry: &'a ResolvedPackRegistry,
    workflow_ref: &str,
) -> Option<&'a ResolvedPackRegistryEntry> {
    let trimmed = workflow_ref.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Workflow config resolution is case-insensitive today, so ownership lookup stays aligned
    // here to avoid accepting a ref during config expansion but skipping pack activation checks.
    let direct_match = registry.entries.iter().find(|entry| {
        entry.loaded_manifest().is_some_and(|pack| {
            pack.manifest.workflows.exports.iter().any(|export| export.eq_ignore_ascii_case(trimmed))
        })
    });
    if direct_match.is_some() {
        return direct_match;
    }

    let (pack_id, _) = trimmed.split_once('/')?;
    registry
        .entries
        .iter()
        .find(|entry| entry.loaded_manifest().is_some() && entry.pack_id.eq_ignore_ascii_case(pack_id))
}

pub fn ensure_pack_execution_requirements(pack: &LoadedPackManifest) -> Result<crate::PackRuntimeReport> {
    let report = crate::ensure_pack_runtime_requirements(pack)?;
    ensure_pack_secrets_available(pack)?;
    Ok(report)
}

fn validate_pack_dependency_policy(
    pack: &LoadedPackManifest,
    active_by_id: &BTreeMap<String, &ResolvedPackRegistryEntry>,
) -> Result<()> {
    for dependency in &pack.manifest.dependencies {
        let dependency_id = dependency.id.trim();
        let Some(active_dependency) = active_by_id.get(&dependency_id.to_ascii_lowercase()) else {
            if dependency.optional {
                continue;
            }
            return Err(anyhow!(
                "pack '{}' requires dependency '{}' but it is not active",
                pack.manifest.id,
                dependency_id
            ));
        };

        if let Some(version) = dependency.version.as_deref() {
            let requirement = VersionReq::parse(version.trim()).with_context(|| {
                format!(
                    "pack '{}' dependency '{}' has invalid version requirement '{}'",
                    pack.manifest.id,
                    dependency_id,
                    version.trim()
                )
            })?;
            let resolved = Version::parse(&active_dependency.version).with_context(|| {
                format!(
                    "active dependency '{}' has invalid version '{}'",
                    active_dependency.pack_id, active_dependency.version
                )
            })?;
            if !requirement.matches(&resolved) {
                if dependency.optional {
                    continue;
                }
                return Err(anyhow!(
                    "pack '{}' requires dependency '{}' version '{}' but active version is '{}'",
                    pack.manifest.id,
                    dependency_id,
                    version.trim(),
                    active_dependency.version
                ));
            }
        }
    }

    Ok(())
}

fn validate_pack_permission_policy(pack: &LoadedPackManifest) -> Result<()> {
    let declared_tools = pack
        .manifest
        .permissions
        .tools
        .iter()
        .map(|tool| tool.trim().to_ascii_lowercase())
        .collect::<std::collections::BTreeSet<_>>();
    let declared_namespaces = pack
        .manifest
        .permissions
        .mcp_namespaces
        .iter()
        .map(|namespace| namespace.trim().to_ascii_lowercase())
        .collect::<std::collections::BTreeSet<_>>();

    let workflow_overlay = load_pack_workflow_overlay(pack, &empty_workflow_overlay_base())?;
    if let Some(overlay) = workflow_overlay {
        for tool in overlay.tools_allowlist {
            let normalized = tool.trim().to_ascii_lowercase();
            if normalized.is_empty() {
                continue;
            }
            if !declared_tools.contains(&normalized) {
                return Err(anyhow!(
                    "pack '{}' workflow overlay allowlists tool '{}' without declaring it in permissions.tools",
                    pack.manifest.id,
                    tool
                ));
            }
        }
    }

    if let Some(overlay) = load_pack_agent_runtime_overlay(pack)? {
        for tool in overlay.tools_allowlist {
            let normalized = tool.trim().to_ascii_lowercase();
            if normalized.is_empty() {
                continue;
            }
            if !declared_tools.contains(&normalized) {
                return Err(anyhow!(
                    "pack '{}' runtime overlay allowlists tool '{}' without declaring it in permissions.tools",
                    pack.manifest.id,
                    tool
                ));
            }
        }
    }

    let mcp_overlay = crate::load_pack_mcp_overlay(pack)?;
    if !mcp_overlay.servers.is_empty() {
        let mut required_namespaces = std::collections::BTreeSet::new();
        required_namespaces.insert(pack.manifest.id.to_ascii_lowercase());
        for definition in mcp_overlay.servers.values() {
            if let Some(namespace) = definition.config.get("tool_namespace").and_then(|value| value.as_str()) {
                required_namespaces.insert(namespace.trim().to_ascii_lowercase());
            }
        }

        for namespace in required_namespaces {
            if !declared_namespaces.contains(&namespace) {
                return Err(anyhow!(
                    "pack '{}' uses MCP namespace '{}' without declaring it in permissions.mcp_namespaces",
                    pack.manifest.id,
                    namespace
                ));
            }
        }
    }

    Ok(())
}

fn validate_pack_secret_policy(pack: &LoadedPackManifest) -> Result<()> {
    let declared_required = pack
        .manifest
        .secrets
        .required
        .iter()
        .map(|secret| secret.trim().to_string())
        .collect::<std::collections::BTreeSet<_>>();
    let declared_optional = pack
        .manifest
        .secrets
        .optional
        .iter()
        .map(|secret| secret.trim().to_string())
        .collect::<std::collections::BTreeSet<_>>();

    let mcp_overlay = crate::load_pack_mcp_overlay(pack)?;
    for definition in mcp_overlay.servers.values() {
        let required_env =
            definition.config.get("required_env").and_then(|value| value.as_array()).cloned().unwrap_or_default();

        for key in required_env {
            let Some(secret_name) = key.as_str().map(str::trim).filter(|value| !value.is_empty()) else {
                continue;
            };

            if !declared_required.contains(secret_name) {
                return Err(anyhow!(
                    "pack '{}' MCP server requires env '{}' without declaring it in secrets.required",
                    pack.manifest.id,
                    secret_name
                ));
            }
        }
    }

    for secret_name in &declared_optional {
        if secret_name.is_empty() {
            return Err(anyhow!("pack '{}' declares an empty optional secret", pack.manifest.id));
        }
    }

    Ok(())
}

fn ensure_pack_secrets_available(pack: &LoadedPackManifest) -> Result<()> {
    for secret_name in &pack.manifest.secrets.required {
        let secret_name = secret_name.trim();
        if secret_name.is_empty() {
            continue;
        }
        if std::env::var_os(secret_name).is_none() {
            return Err(anyhow!(
                "pack '{}' requires secret '{}' but it is not available in the environment",
                pack.manifest.id,
                secret_name
            ));
        }
    }

    Ok(())
}

fn empty_workflow_overlay_base() -> WorkflowConfig {
    WorkflowConfig {
        schema: crate::WORKFLOW_CONFIG_SCHEMA_ID.to_string(),
        version: crate::WORKFLOW_CONFIG_VERSION,
        default_workflow_ref: String::new(),
        phase_catalog: BTreeMap::new(),
        workflows: Vec::new(),
        checkpoint_retention: crate::WorkflowCheckpointRetentionConfig::default(),
        phase_definitions: BTreeMap::new(),
        agent_profiles: BTreeMap::new(),
        tools_allowlist: Vec::new(),
        mcp_servers: BTreeMap::new(),
        phase_mcp_bindings: BTreeMap::new(),
        tools: BTreeMap::new(),
        integrations: None,
        schedules: Vec::new(),
        daemon: None,
    }
}

fn collect_pack_workflow_yaml_sources(pack: &LoadedPackManifest) -> Result<Vec<(PathBuf, String)>> {
    let mut sources = Vec::new();
    let workflows_root = pack.pack_root.join(&pack.manifest.workflows.root);
    if workflows_root.is_dir() {
        let mut entries = fs::read_dir(&workflows_root)
            .with_context(|| format!("failed to read pack workflows directory {}", workflows_root.display()))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().map(|ext| ext == "yaml" || ext == "yml").unwrap_or(false))
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.path());

        for entry in entries {
            let path = entry.path();
            let content = fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
            sources.push((path, content));
        }
    }

    if let Some(overlay_path) = pack.manifest.runtime.workflow_overlay.as_deref() {
        let path = pack.pack_root.join(overlay_path);
        let content = fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
        sources.push((path, content));
    }

    Ok(sources)
}

fn resolve_pack_workflow_assets(pack: &LoadedPackManifest, workflow: &mut WorkflowConfig) -> Result<()> {
    for (phase_id, definition) in &mut workflow.phase_definitions {
        resolve_phase_command_assets(pack, phase_id, definition)?;
    }
    for (tool_id, definition) in &mut workflow.tools {
        definition.executable =
            resolve_pack_asset_path(pack, &definition.executable, &format!("tools['{}'].executable", tool_id))?;
    }
    Ok(())
}

fn resolve_pack_agent_runtime_assets(pack: &LoadedPackManifest, overlay: &mut AgentRuntimeOverlay) -> Result<()> {
    for (phase_id, definition) in &mut overlay.phases {
        resolve_phase_command_assets(pack, phase_id, definition)?;
    }
    for (tool_id, definition) in &mut overlay.cli_tools {
        resolve_cli_tool_assets(pack, tool_id, definition)?;
    }
    Ok(())
}

fn resolve_phase_command_assets(
    pack: &LoadedPackManifest,
    phase_id: &str,
    definition: &mut PhaseExecutionDefinition,
) -> Result<()> {
    let Some(command) = definition.command.as_mut() else {
        return Ok(());
    };

    command.program =
        resolve_pack_asset_path(pack, &command.program, &format!("phases['{}'].command.program", phase_id))?;
    Ok(())
}

fn resolve_cli_tool_assets(pack: &LoadedPackManifest, tool_id: &str, definition: &mut CliToolConfig) -> Result<()> {
    let Some(executable) = definition.executable.as_deref() else {
        return Ok(());
    };

    definition.executable =
        Some(resolve_pack_asset_path(pack, executable, &format!("cli_tools['{}'].executable", tool_id))?);
    Ok(())
}

fn resolve_pack_asset_path(pack: &LoadedPackManifest, raw: &str, field: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !looks_like_pack_relative_asset(trimmed) {
        return Ok(raw.to_string());
    }

    let relative = Path::new(trimmed);
    if relative.components().any(|component| matches!(component, std::path::Component::ParentDir)) {
        return Err(anyhow!("{} path '{}' cannot escape pack root", field, raw));
    }

    let resolved = pack.pack_root.join(relative);
    if !resolved.exists() {
        return Err(anyhow!("{} path '{}' is missing from pack '{}'", field, raw, pack.manifest.id));
    }

    let canonical_pack_root = fs::canonicalize(&pack.pack_root)
        .with_context(|| format!("failed to canonicalize pack root {}", pack.pack_root.display()))?;
    let canonical =
        fs::canonicalize(&resolved).with_context(|| format!("failed to canonicalize {} path '{}'", field, raw))?;
    if !canonical.starts_with(&canonical_pack_root) {
        return Err(anyhow!("{} path '{}' escapes pack root", field, raw));
    }

    Ok(canonical.display().to_string())
}

fn looks_like_pack_relative_asset(raw: &str) -> bool {
    let path = Path::new(raw);
    if path.is_absolute() {
        return false;
    }

    raw.starts_with('.') || path.components().count() > 1
}

fn read_child_directories(root: &Path) -> Result<Vec<PathBuf>> {
    Ok(fs::read_dir(root)
        .with_context(|| format!("failed to read directory {}", root.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{env_lock, EnvVarGuard};

    fn write_pack_fixture(root: &Path, pack_id: &str, version: &str, description: &str, extra_workflow: &str) {
        fs::create_dir_all(root.join("workflows")).expect("create workflows");
        fs::create_dir_all(root.join("runtime")).expect("create runtime");

        let manifest = format!(
            r#"
schema = "ao.pack.v1"
id = "{pack_id}"
version = "{version}"
kind = "domain-pack"
title = "Pack {pack_id}"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
ao_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["ao.task"]
default_kind = "ao.task"

[workflows]
root = "workflows"
exports = ["{pack_id}/standard"]

[runtime]
workflow_overlay = "runtime/workflow-runtime.overlay.yaml"
"#
        );
        fs::write(root.join(crate::PACK_MANIFEST_FILE_NAME), manifest).expect("write manifest");
        fs::write(
            root.join("runtime/workflow-runtime.overlay.yaml"),
            format!(
                r#"
workflows:
  - id: {pack_id}/standard
    name: "{pack_id}/standard"
    phases:
      - workflow_ref: standard
  - id: standard
    name: Standard
    description: "{description}"
    phases:
      - requirements
      - implementation
  - id: {extra_workflow}
    name: "{extra_workflow}"
    phases:
      - requirements
      - testing
"#
            ),
        )
        .expect("write workflow overlay");
    }

    fn write_pack_with_command_assets(root: &Path, pack_id: &str, version: &str) {
        fs::create_dir_all(root.join("workflows")).expect("create workflows");
        fs::create_dir_all(root.join("runtime")).expect("create runtime");
        fs::create_dir_all(root.join("assets")).expect("create assets");
        fs::write(root.join("assets/review-helper.sh"), "#!/bin/sh\nexit 0\n").expect("write helper");

        fs::write(
            root.join(crate::PACK_MANIFEST_FILE_NAME),
            format!(
                r#"
schema = "ao.pack.v1"
id = "{pack_id}"
version = "{version}"
kind = "domain-pack"
title = "Pack {pack_id}"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
ao_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["ao.task"]
default_kind = "ao.task"

[workflows]
root = "workflows"
exports = ["{pack_id}/review-pack"]

[runtime]
workflow_overlay = "runtime/workflow-runtime.overlay.yaml"
"#
            ),
        )
        .expect("write manifest");
        fs::write(
            root.join("runtime/workflow-runtime.overlay.yaml"),
            r#"
phases:
  pack-review:
    mode: command
    command:
      program: ./assets/review-helper.sh
      cwd_mode: path
      cwd_path: workspace/scripts
tools:
  pack-tool:
    executable: ./assets/review-helper.sh
    supports_mcp: false
    supports_write: false
workflows:
  - id: review-pack
    name: "review-pack"
    phases:
      - pack-review
"#,
        )
        .expect("write workflow overlay");
    }

    fn write_pack_manifest(root: &Path, manifest: &str) {
        fs::create_dir_all(root.join("workflows")).expect("create workflows");
        fs::create_dir_all(root.join("runtime")).expect("create runtime");
        fs::write(root.join(crate::PACK_MANIFEST_FILE_NAME), manifest).expect("write manifest");
    }

    #[test]
    fn resolve_pack_registry_prefers_project_overrides_and_latest_installed_version() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        write_pack_fixture(
            &machine_installed_packs_dir().join("ao.review").join("0.1.0"),
            "ao.review",
            "0.1.0",
            "old installed",
            "review-old",
        );
        write_pack_fixture(
            &machine_installed_packs_dir().join("ao.review").join("0.2.0"),
            "ao.review",
            "0.2.0",
            "new installed",
            "review-new",
        );
        write_pack_fixture(
            &machine_installed_packs_dir().join("ao.task").join("1.0.0"),
            "ao.task",
            "1.0.0",
            "installed task",
            "task-installed",
        );
        write_pack_fixture(
            &project_pack_overrides_dir(project.path()).join("ao-task"),
            "ao.task",
            "9.0.0",
            "project override task",
            "task-override",
        );

        let registry = resolve_pack_registry(project.path()).expect("resolve registry");
        assert_eq!(registry.entries[0].source, PackRegistrySource::Bundled);

        let review = registry.resolve("ao.review").expect("ao.review should resolve");
        assert_eq!(review.source, PackRegistrySource::Installed);
        assert_eq!(review.version, "0.2.0");

        let task = registry.resolve("ao.task").expect("ao.task should resolve");
        assert_eq!(task.source, PackRegistrySource::ProjectOverride);
        assert_eq!(task.version, "9.0.0");
    }

    #[test]
    fn resolve_pack_registry_honors_project_selection_pins_and_disablement() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        write_pack_fixture(
            &machine_installed_packs_dir().join("ao.review").join("0.1.0"),
            "ao.review",
            "0.1.0",
            "old installed",
            "review-old",
        );
        write_pack_fixture(
            &machine_installed_packs_dir().join("ao.review").join("0.2.0"),
            "ao.review",
            "0.2.0",
            "new installed",
            "review-new",
        );
        write_pack_fixture(
            &project_pack_overrides_dir(project.path()).join("ao-task"),
            "ao.task",
            "9.0.0",
            "project override task",
            "task-override",
        );

        crate::save_pack_selection_state(
            project.path(),
            &crate::PackSelectionState {
                schema: crate::PACK_SELECTION_SCHEMA_ID.to_string(),
                selections: vec![
                    crate::PackSelectionEntry {
                        pack_id: "ao.review".to_string(),
                        version: Some("=0.1.0".to_string()),
                        source: Some(crate::PackSelectionSource::Installed),
                        enabled: true,
                    },
                    crate::PackSelectionEntry {
                        pack_id: "ao.task".to_string(),
                        version: None,
                        source: Some(crate::PackSelectionSource::ProjectOverride),
                        enabled: false,
                    },
                    crate::PackSelectionEntry {
                        pack_id: "ao.requirement".to_string(),
                        version: None,
                        source: Some(crate::PackSelectionSource::Bundled),
                        enabled: false,
                    },
                ],
            },
        )
        .expect("selection state should save");

        let registry = resolve_pack_registry(project.path()).expect("resolve registry");
        let review = registry.resolve("ao.review").expect("ao.review should resolve");
        assert_eq!(review.version, "0.1.0");
        assert!(registry.resolve("ao.task").is_none(), "disabled pack should not resolve");
    }

    #[test]
    fn workflow_loading_uses_installed_then_ad_hoc_then_project_override_precedence() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        write_pack_fixture(
            &machine_installed_packs_dir().join("ao.review").join("0.2.0"),
            "ao.review",
            "0.2.0",
            "installed review",
            "review-installed",
        );
        write_pack_fixture(
            &machine_installed_packs_dir().join("ao.task").join("1.0.0"),
            "ao.task",
            "1.0.0",
            "installed task",
            "task-installed",
        );
        write_pack_fixture(
            &project_pack_overrides_dir(project.path()).join("ao-task"),
            "ao.task",
            "9.0.0",
            "project override task",
            "task-override",
        );

        fs::create_dir_all(project.path().join(".ao")).expect("create project .ao");
        fs::write(
            project.path().join(".ao").join("workflows.yaml"),
            r#"
workflows:
  - id: standard
    name: Standard
    description: "project ad hoc"
    phases:
      - requirements
      - testing
  - id: adhoc-only
    name: "adhoc-only"
    phases:
      - requirements
      - code-review
"#,
        )
        .expect("write project workflows");

        let loaded = crate::load_workflow_config_with_metadata(project.path()).expect("load effective workflow config");
        let workflows = loaded.config.workflows.iter().map(|workflow| workflow.id.as_str()).collect::<Vec<_>>();
        let standard = loaded
            .config
            .workflows
            .iter()
            .find(|workflow| workflow.id == "standard")
            .expect("standard workflow should exist");

        assert_eq!(standard.description, "project override task");
        assert!(workflows.contains(&"review-installed"));
        assert!(workflows.contains(&"task-override"));
        assert!(workflows.contains(&"adhoc-only"));
        assert!(!workflows.contains(&"task-installed"));
    }

    #[test]
    fn workflow_loading_prefers_project_yaml_over_installed_pack_definitions() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        write_pack_fixture(
            &machine_installed_packs_dir().join("ao.task").join("1.0.0"),
            "ao.task",
            "1.0.0",
            "installed task",
            "task-installed",
        );

        fs::create_dir_all(project.path().join(".ao")).expect("create project .ao");
        fs::write(
            project.path().join(".ao").join("workflows.yaml"),
            r#"
workflows:
  - id: standard
    name: Standard
    description: "project ad hoc"
    phases:
      - requirements
      - testing
"#,
        )
        .expect("write project workflows");

        let loaded = crate::load_workflow_config_with_metadata(project.path()).expect("load effective workflow config");
        let standard = loaded
            .config
            .workflows
            .iter()
            .find(|workflow| workflow.id == "standard")
            .expect("standard workflow should exist");

        assert_eq!(standard.description, "project ad hoc");
    }

    #[test]
    fn load_pack_workflow_overlay_resolves_pack_relative_command_assets() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_pack_with_command_assets(temp.path(), "ao.review", "0.2.0");

        let pack = load_pack_manifest(temp.path()).expect("load pack");
        let overlay = load_pack_workflow_overlay(&pack, &crate::workflow_config::builtin_workflow_config())
            .expect("load overlay");
        let overlay = overlay.expect("overlay should be present");
        let command = overlay
            .phase_definitions
            .get("pack-review")
            .and_then(|definition| definition.command.as_ref())
            .expect("pack-review command");
        let tool = overlay.tools.get("pack-tool").expect("pack tool");

        assert!(command.program.ends_with("assets/review-helper.sh"));
        assert_eq!(command.cwd_path.as_deref(), Some("workspace/scripts"));
        assert!(tool.executable.ends_with("assets/review-helper.sh"));
    }

    #[test]
    fn resolve_active_pack_for_workflow_ref_matches_pack_qualified_internal_refs() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        write_pack_fixture(
            &machine_installed_packs_dir().join("ao.review").join("0.2.0"),
            "ao.review",
            "0.2.0",
            "installed review",
            "ao.review/internal",
        );

        let registry = resolve_pack_registry(project.path()).expect("resolve registry");
        let entry = resolve_active_pack_for_workflow_ref(&registry, "ao.review/internal")
            .expect("pack-qualified internal workflow should resolve to owning pack");
        assert_eq!(entry.pack_id, "ao.review");
    }

    #[test]
    fn resolve_active_pack_for_workflow_ref_tracks_case_insensitive_workflow_exports() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        write_pack_fixture(
            &machine_installed_packs_dir().join("ao.review").join("0.2.0"),
            "ao.review",
            "0.2.0",
            "installed review",
            "review-installed",
        );

        let registry = resolve_pack_registry(project.path()).expect("resolve registry");
        let entry = resolve_active_pack_for_workflow_ref(&registry, "AO.REVIEW/STANDARD")
            .expect("workflow export matching should remain aligned with workflow config resolution");
        assert_eq!(entry.pack_id, "ao.review");
    }

    #[test]
    fn load_workflow_config_rejects_missing_required_dependencies() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        write_pack_manifest(
            &machine_installed_packs_dir().join("ao.dependent").join("0.2.0"),
            r#"
schema = "ao.pack.v1"
id = "ao.dependent"
version = "0.2.0"
kind = "domain-pack"
title = "Dependent"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
ao_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["ao.task"]
default_kind = "ao.task"

[workflows]
root = "workflows"
exports = ["ao.dependent/standard"]

[runtime]

[[dependencies]]
id = "ao.missing"
version = ">=1.0.0"
"#,
        );
        fs::write(
            machine_installed_packs_dir()
                .join("ao.dependent")
                .join("0.2.0")
                .join("runtime/workflow-runtime.overlay.yaml"),
            "workflows: []\n",
        )
        .expect("write workflow overlay");

        let error =
            crate::load_workflow_config_with_metadata(project.path()).expect_err("missing dependency should fail");
        assert!(error.to_string().contains("requires dependency 'ao.missing'"));
    }

    #[test]
    fn load_workflow_config_rejects_undeclared_tool_permissions() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        write_pack_manifest(
            &machine_installed_packs_dir().join("ao.research").join("0.2.0"),
            r#"
schema = "ao.pack.v1"
id = "ao.research"
version = "0.2.0"
kind = "domain-pack"
title = "Research"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
ao_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["ao.task"]
default_kind = "ao.task"

[workflows]
root = "workflows"
exports = ["ao.research/standard"]

[runtime]
workflow_overlay = "runtime/workflow-runtime.overlay.yaml"

[permissions]
tools = ["cargo"]
"#,
        );
        fs::write(
            machine_installed_packs_dir()
                .join("ao.research")
                .join("0.2.0")
                .join("runtime/workflow-runtime.overlay.yaml"),
            "tools_allowlist:\n  - npm\nworkflows: []\n",
        )
        .expect("write workflow overlay");

        let error = crate::load_workflow_config_with_metadata(project.path())
            .expect_err("undeclared tool permission should fail");
        assert!(error.to_string().contains("without declaring it in permissions.tools"));
    }

    #[test]
    fn resolve_pack_registry_allows_missing_required_secrets_until_activation() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let _secret_guard = EnvVarGuard::unset("PACK_SECRET_TOKEN");

        let pack_root = machine_installed_packs_dir().join("ao.secret").join("0.2.0");
        write_pack_manifest(
            &pack_root,
            r#"
schema = "ao.pack.v1"
id = "ao.secret"
version = "0.2.0"
kind = "domain-pack"
title = "Secret"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
ao_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["ao.task"]
default_kind = "ao.task"

[workflows]
root = "workflows"
exports = ["ao.secret/standard"]

[runtime]

[mcp]
servers = "mcp/servers.toml"

[permissions]
mcp_namespaces = ["ao.secret"]

[secrets]
required = ["PACK_SECRET_TOKEN"]
"#,
        );
        fs::create_dir_all(pack_root.join("mcp")).expect("create mcp dir");
        fs::write(
            pack_root.join("mcp/servers.toml"),
            r#"
[[server]]
id = "secret"
command = "secret-mcp"
required_env = ["PACK_SECRET_TOKEN"]
"#,
        )
        .expect("write mcp servers");

        let registry = resolve_pack_registry(project.path()).expect("registry resolution should not require secrets");
        assert!(registry.resolve("ao.secret").is_some(), "secret pack should still be active");
        crate::load_workflow_config_with_metadata(project.path())
            .expect("workflow config loading should not require secrets");
    }

    #[test]
    fn resolve_pack_registry_allows_missing_required_runtime_until_execution() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        let pack_root = machine_installed_packs_dir().join("ao.runtime").join("0.2.0");
        write_pack_manifest(
            &pack_root,
            r#"
schema = "ao.pack.v1"
id = "ao.runtime"
version = "0.2.0"
kind = "domain-pack"
title = "Runtime"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
ao_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["custom"]
default_kind = "custom"

[workflows]
root = "workflows"
exports = ["ao.runtime/run"]

[runtime]
workflow_overlay = "runtime/workflow-runtime.overlay.yaml"

[[runtime.requirements]]
runtime = "node"
binary = "missing-node-binary"
optional = false
"#,
        );
        fs::write(
            pack_root.join("runtime/workflow-runtime.overlay.yaml"),
            r#"
phase_catalog:
  runtime-check:
    label: Runtime Check
    category: verification
workflows:
  - id: ao.runtime/run
    name: Runtime Run
    phases:
      - runtime-check
"#,
        )
        .expect("write workflow overlay");

        let registry = resolve_pack_registry(project.path()).expect("registry resolution should not probe runtimes");
        assert!(registry.resolve("ao.runtime").is_some(), "runtime pack should still be active");
        crate::load_workflow_config_with_metadata(project.path())
            .expect("workflow config loading should not probe runtimes");
    }
}
