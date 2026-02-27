use anyhow::{anyhow, Context, Result};
use orchestrator_core::{
    ArchitectureEdge, ArchitectureEntity, ArchitectureGraph, OrchestratorTask,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    conflict_error, invalid_input_error, not_found_error, parse_input_json_or, print_ok,
    print_value, ArchitectureCommand, ArchitectureEdgeCommand, ArchitectureEdgeCreateArgs,
    ArchitectureEntityCommand, ArchitectureEntityCreateArgs, ArchitectureEntityUpdateArgs, IdArgs,
};

#[derive(Debug, Clone, Deserialize)]
struct ArchitectureEntityCreateInputCli {
    id: String,
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    code_paths: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    metadata: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ArchitectureEntityUpdateInputCli {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    clear_description: Option<bool>,
    #[serde(default)]
    code_paths: Option<Vec<String>>,
    #[serde(default)]
    replace_code_paths: Option<bool>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    replace_tags: Option<bool>,
    #[serde(default)]
    metadata: Option<HashMap<String, Value>>,
}

#[derive(Debug, Clone, Deserialize)]
struct ArchitectureEdgeCreateInputCli {
    #[serde(default)]
    id: Option<String>,
    from: String,
    to: String,
    relation: String,
    #[serde(default)]
    rationale: Option<String>,
    #[serde(default)]
    metadata: HashMap<String, Value>,
}

fn core_state_path(project_root: &str) -> PathBuf {
    Path::new(project_root).join(".ao").join("core-state.json")
}

fn architecture_docs_path(project_root: &str) -> PathBuf {
    Path::new(project_root)
        .join(".ao")
        .join("docs")
        .join("architecture.json")
}

fn load_core_state_value(project_root: &str) -> Result<Value> {
    let path = core_state_path(project_root);
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }

    let content = fs::read_to_string(&path)?;
    serde_json::from_str(&content).with_context(|| {
        format!(
            "failed to parse JSON at {}; file is likely corrupt",
            path.display()
        )
    })
}

fn save_core_state_value(project_root: &str, state: &Value) -> Result<()> {
    let path = core_state_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

fn load_architecture_graph(project_root: &str) -> Result<ArchitectureGraph> {
    let state = load_core_state_value(project_root)?;
    let graph = state
        .get("architecture")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Ok(serde_json::from_value(graph).unwrap_or_default())
}

fn extract_tasks(state: &Value) -> HashMap<String, OrchestratorTask> {
    state
        .get("tasks")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

fn validate_architecture_graph(
    graph: &ArchitectureGraph,
    tasks: &HashMap<String, OrchestratorTask>,
) -> Result<()> {
    let mut entity_ids = HashSet::new();
    for entity in &graph.entities {
        if entity.id.trim().is_empty() {
            return Err(anyhow!("architecture entity id cannot be empty"));
        }
        if entity.name.trim().is_empty() {
            return Err(anyhow!(
                "architecture entity name cannot be empty (id={})",
                entity.id
            ));
        }
        if !entity_ids.insert(entity.id.clone()) {
            return Err(anyhow!(
                "duplicate architecture entity id found: {}",
                entity.id
            ));
        }
    }

    let mut edge_ids = HashSet::new();
    for edge in &graph.edges {
        if edge.id.trim().is_empty() {
            return Err(anyhow!("architecture edge id cannot be empty"));
        }
        if edge.from.trim().is_empty() || edge.to.trim().is_empty() {
            return Err(anyhow!(
                "architecture edge endpoints cannot be empty (edge={})",
                edge.id
            ));
        }
        if edge.relation.trim().is_empty() {
            return Err(anyhow!(
                "architecture edge relation cannot be empty (edge={})",
                edge.id
            ));
        }
        if !entity_ids.contains(&edge.from) {
            return Err(anyhow!(
                "architecture edge '{}' references unknown from entity '{}'",
                edge.id,
                edge.from
            ));
        }
        if !entity_ids.contains(&edge.to) {
            return Err(anyhow!(
                "architecture edge '{}' references unknown to entity '{}'",
                edge.id,
                edge.to
            ));
        }
        if !edge_ids.insert(edge.id.clone()) {
            return Err(anyhow!("duplicate architecture edge id found: {}", edge.id));
        }
    }

    for task in tasks.values() {
        for entity_id in &task.linked_architecture_entities {
            if !entity_ids.contains(entity_id) {
                return Err(anyhow!(
                    "task {} links unknown architecture entity '{}'",
                    task.id,
                    entity_id
                ));
            }
        }
    }

    Ok(())
}

fn save_architecture_graph(project_root: &str, graph: &ArchitectureGraph) -> Result<()> {
    let mut state = load_core_state_value(project_root)?;
    let tasks = extract_tasks(&state);
    validate_architecture_graph(graph, &tasks)?;

    let state_obj = state
        .as_object_mut()
        .ok_or_else(|| anyhow!("invalid core state shape"))?;
    state_obj.insert("architecture".to_string(), serde_json::to_value(graph)?);
    save_core_state_value(project_root, &state)?;

    let docs_path = architecture_docs_path(project_root);
    if let Some(parent) = docs_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(docs_path, serde_json::to_string_pretty(graph)?)?;
    Ok(())
}

fn normalize_identifier(raw: &str) -> String {
    raw.chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => ch.to_ascii_lowercase(),
            '-' | '_' | ' ' | '/' | '.' => '-',
            _ => '-',
        })
        .collect::<String>()
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn next_edge_id(graph: &ArchitectureGraph, from: &str, relation: &str, to: &str) -> String {
    let base = normalize_identifier(&format!("{from}-{relation}-{to}"));
    let base = if base.is_empty() {
        "edge".to_string()
    } else {
        base
    };

    let existing: HashSet<&str> = graph.edges.iter().map(|edge| edge.id.as_str()).collect();
    if !existing.contains(base.as_str()) {
        return base;
    }

    let mut index = 2usize;
    loop {
        let candidate = format!("{base}-{index}");
        if !existing.contains(candidate.as_str()) {
            return candidate;
        }
        index = index.saturating_add(1);
    }
}

fn create_entity(
    project_root: &str,
    args: ArchitectureEntityCreateArgs,
) -> Result<ArchitectureEntity> {
    let input = parse_input_json_or(args.input_json, || {
        Ok(ArchitectureEntityCreateInputCli {
            id: args.id,
            name: args.name,
            kind: args.kind,
            description: args.description,
            code_paths: args.code_path,
            tags: args.tag,
            metadata: HashMap::new(),
        })
    })?;

    if input.id.trim().is_empty() {
        return Err(invalid_input_error("architecture entity id is required"));
    }
    if input.name.trim().is_empty() {
        return Err(invalid_input_error("architecture entity name is required"));
    }

    let mut graph = load_architecture_graph(project_root)?;
    if graph.entities.iter().any(|entity| entity.id == input.id) {
        return Err(conflict_error(format!(
            "architecture entity already exists: {}",
            input.id
        )));
    }

    let entity = ArchitectureEntity {
        id: input.id,
        name: input.name,
        kind: input.kind.unwrap_or_else(|| "module".to_string()),
        description: input.description,
        code_paths: input.code_paths,
        tags: input.tags,
        metadata: input.metadata,
    };

    graph.entities.push(entity.clone());
    save_architecture_graph(project_root, &graph)?;
    Ok(entity)
}

fn update_entity(
    project_root: &str,
    args: ArchitectureEntityUpdateArgs,
) -> Result<ArchitectureEntity> {
    let input = parse_input_json_or(args.input_json, || {
        Ok(ArchitectureEntityUpdateInputCli {
            name: args.name,
            kind: args.kind,
            description: args.description,
            clear_description: Some(args.clear_description),
            code_paths: if args.replace_code_paths || !args.code_path.is_empty() {
                Some(args.code_path)
            } else {
                None
            },
            replace_code_paths: Some(args.replace_code_paths),
            tags: if args.replace_tags || !args.tag.is_empty() {
                Some(args.tag)
            } else {
                None
            },
            replace_tags: Some(args.replace_tags),
            metadata: None,
        })
    })?;

    let mut graph = load_architecture_graph(project_root)?;
    let entity = graph
        .entities
        .iter_mut()
        .find(|entity| entity.id == args.id)
        .ok_or_else(|| not_found_error(format!("architecture entity not found: {}", args.id)))?;

    if let Some(name) = input.name {
        if name.trim().is_empty() {
            return Err(invalid_input_error(
                "architecture entity name cannot be empty",
            ));
        }
        entity.name = name;
    }
    if let Some(kind) = input.kind {
        entity.kind = kind;
    }

    if input.clear_description.unwrap_or(false) {
        entity.description = None;
    } else if let Some(description) = input.description {
        entity.description = Some(description);
    }

    if let Some(code_paths) = input.code_paths {
        if input.replace_code_paths.unwrap_or(false) {
            entity.code_paths = code_paths;
        } else {
            for code_path in code_paths {
                if !entity
                    .code_paths
                    .iter()
                    .any(|existing| existing == &code_path)
                {
                    entity.code_paths.push(code_path);
                }
            }
        }
    }

    if let Some(tags) = input.tags {
        if input.replace_tags.unwrap_or(false) {
            entity.tags = tags;
        } else {
            for tag in tags {
                if !entity.tags.iter().any(|existing| existing == &tag) {
                    entity.tags.push(tag);
                }
            }
        }
    }

    if let Some(metadata) = input.metadata {
        entity.metadata = metadata;
    }

    let updated = entity.clone();
    save_architecture_graph(project_root, &graph)?;
    Ok(updated)
}

fn delete_entity(project_root: &str, args: IdArgs) -> Result<()> {
    let mut graph = load_architecture_graph(project_root)?;
    if graph
        .edges
        .iter()
        .any(|edge| edge.from == args.id || edge.to == args.id)
    {
        return Err(anyhow!(
            "cannot delete architecture entity '{}' while edges still reference it",
            args.id
        ));
    }

    let before = graph.entities.len();
    graph.entities.retain(|entity| entity.id != args.id);
    if graph.entities.len() == before {
        return Err(not_found_error(format!(
            "architecture entity not found: {}",
            args.id
        )));
    }

    save_architecture_graph(project_root, &graph)
}

fn create_edge(project_root: &str, args: ArchitectureEdgeCreateArgs) -> Result<ArchitectureEdge> {
    let input = parse_input_json_or(args.input_json, || {
        Ok(ArchitectureEdgeCreateInputCli {
            id: args.id,
            from: args.from,
            to: args.to,
            relation: args.relation,
            rationale: args.rationale,
            metadata: HashMap::new(),
        })
    })?;

    if input.from.trim().is_empty() || input.to.trim().is_empty() {
        return Err(invalid_input_error(
            "architecture edge endpoints are required",
        ));
    }
    if input.relation.trim().is_empty() {
        return Err(invalid_input_error(
            "architecture edge relation is required",
        ));
    }

    let mut graph = load_architecture_graph(project_root)?;
    if !graph.has_entity(&input.from) {
        return Err(not_found_error(format!(
            "architecture edge references unknown from entity: {}",
            input.from
        )));
    }
    if !graph.has_entity(&input.to) {
        return Err(not_found_error(format!(
            "architecture edge references unknown to entity: {}",
            input.to
        )));
    }

    let edge_id = input
        .id
        .unwrap_or_else(|| next_edge_id(&graph, &input.from, &input.relation, &input.to));
    if graph.edges.iter().any(|edge| edge.id == edge_id) {
        return Err(conflict_error(format!(
            "architecture edge already exists: {}",
            edge_id
        )));
    }

    let edge = ArchitectureEdge {
        id: edge_id,
        from: input.from,
        to: input.to,
        relation: input.relation,
        rationale: input.rationale,
        metadata: input.metadata,
    };
    graph.edges.push(edge.clone());
    save_architecture_graph(project_root, &graph)?;
    Ok(edge)
}

fn delete_edge(project_root: &str, args: IdArgs) -> Result<()> {
    let mut graph = load_architecture_graph(project_root)?;
    let before = graph.edges.len();
    graph.edges.retain(|edge| edge.id != args.id);
    if graph.edges.len() == before {
        return Err(not_found_error(format!(
            "architecture edge not found: {}",
            args.id
        )));
    }
    save_architecture_graph(project_root, &graph)
}

fn suggest_paths_for_task(project_root: &str, task_id: &str) -> Result<Value> {
    let state = load_core_state_value(project_root)?;
    let tasks = extract_tasks(&state);
    let task = tasks
        .get(task_id)
        .ok_or_else(|| not_found_error(format!("task not found: {task_id}")))?;
    let graph = load_architecture_graph(project_root)?;

    let mut resolved_entities = Vec::new();
    let mut unresolved_entities = Vec::new();
    let mut recommended_code_paths = Vec::new();

    for linked_id in &task.linked_architecture_entities {
        if let Some(entity) = graph.entities.iter().find(|entity| entity.id == *linked_id) {
            for code_path in &entity.code_paths {
                if !recommended_code_paths
                    .iter()
                    .any(|existing| existing == code_path)
                {
                    recommended_code_paths.push(code_path.clone());
                }
            }
            resolved_entities.push(entity.clone());
        } else {
            unresolved_entities.push(linked_id.clone());
        }
    }

    Ok(json!({
        "task_id": task.id,
        "linked_architecture_entities": task.linked_architecture_entities,
        "resolved_entities": resolved_entities,
        "unresolved_entities": unresolved_entities,
        "recommended_code_paths": recommended_code_paths,
    }))
}

pub(crate) async fn handle_architecture(
    command: ArchitectureCommand,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        ArchitectureCommand::Get => print_value(load_architecture_graph(project_root)?, json),
        ArchitectureCommand::Set(args) => {
            let graph = serde_json::from_str::<ArchitectureGraph>(&args.input_json)?;
            save_architecture_graph(project_root, &graph)?;
            print_value(graph, json)
        }
        ArchitectureCommand::Suggest(args) => {
            print_value(suggest_paths_for_task(project_root, &args.task_id)?, json)
        }
        ArchitectureCommand::Entity { command } => match command {
            ArchitectureEntityCommand::List => {
                let graph = load_architecture_graph(project_root)?;
                print_value(graph.entities, json)
            }
            ArchitectureEntityCommand::Get(args) => {
                let graph = load_architecture_graph(project_root)?;
                let entity = graph
                    .entities
                    .into_iter()
                    .find(|entity| entity.id == args.id)
                    .ok_or_else(|| {
                        not_found_error(format!("architecture entity not found: {}", args.id))
                    })?;
                print_value(entity, json)
            }
            ArchitectureEntityCommand::Create(args) => {
                let entity = create_entity(project_root, args)?;
                print_value(entity, json)
            }
            ArchitectureEntityCommand::Update(args) => {
                let entity = update_entity(project_root, args)?;
                print_value(entity, json)
            }
            ArchitectureEntityCommand::Delete(args) => {
                delete_entity(project_root, args)?;
                print_ok("architecture entity deleted", json);
                Ok(())
            }
        },
        ArchitectureCommand::Edge { command } => match command {
            ArchitectureEdgeCommand::List => {
                let graph = load_architecture_graph(project_root)?;
                print_value(graph.edges, json)
            }
            ArchitectureEdgeCommand::Create(args) => {
                let edge = create_edge(project_root, args)?;
                print_value(edge, json)
            }
            ArchitectureEdgeCommand::Delete(args) => {
                delete_edge(project_root, args)?;
                print_ok("architecture edge deleted", json);
                Ok(())
            }
        },
    }
}
