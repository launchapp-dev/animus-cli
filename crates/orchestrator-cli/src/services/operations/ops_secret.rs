use std::fs;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use orchestrator_core::principal::{
    bootstrap_principals_file_if_absent, current_os_username, default_principals_path, load_principals_file,
    resolve_principal_by_id, resolve_principal_for_os_user, PrincipalKind,
};
use orchestrator_core::{keychain_service_name, secrets_index_path, validate_secret_key, SecretStore};
use orchestrator_daemon_runtime::audit::{Audit, AuditActor, AuditEventKind};
use protocol::repository_scope::{repository_scope_for_path, scoped_state_root};
use serde::Serialize;
use serde_json::json;

use crate::{
    print_value, SecretCommand, SecretExportEnvArgs, SecretGetArgs, SecretImportEnvArgs, SecretListArgs, SecretRmArgs,
    SecretSetArgs,
};

pub(crate) async fn handle_secret(
    command: SecretCommand,
    project_root: &str,
    as_principal: Option<String>,
    cli_json: bool,
) -> Result<()> {
    let project_root_path = PathBuf::from(project_root);
    let store = build_store(&project_root_path)?;
    let scoped_root = scoped_state_root(&project_root_path)
        .ok_or_else(|| anyhow!("could not resolve scoped state root for project at {}", project_root_path.display()))?;
    let actor = resolve_actor(as_principal.as_deref());

    match command {
        SecretCommand::Set(args) => handle_set(args, store.as_ref(), &scoped_root, &actor, cli_json),
        SecretCommand::Get(args) => handle_get(args, store.as_ref(), cli_json),
        SecretCommand::List(args) => handle_list(args, store.as_ref(), &project_root_path, cli_json),
        SecretCommand::Rm(args) => handle_rm(args, store.as_ref(), &scoped_root, &actor, cli_json),
        SecretCommand::ImportEnv(args) => {
            handle_import_env(args, store.as_ref(), &project_root_path, &scoped_root, &actor, cli_json)
        }
        SecretCommand::ExportEnv(args) => {
            handle_export_env(args, store.as_ref(), &project_root_path, &scoped_root, &actor, cli_json)
        }
    }
}

/// Store one project-scoped secret in the OS keychain, with the same
/// validation and audit logging as `animus secret set`. Used by the
/// `animus init` walkthrough to migrate detected env-var API keys after
/// an explicit user confirmation (never silently).
pub(crate) fn store_project_secret(project_root: &Path, key: &str, value: &str) -> Result<()> {
    validate_secret_key(key)?;
    if value.is_empty() {
        return Err(anyhow!("secret value for KEY {key:?} is empty; refusing to store"));
    }
    let store = build_store(project_root)?;
    store.set(key, value)?;
    let scoped_root = scoped_state_root(project_root)
        .ok_or_else(|| anyhow!("could not resolve scoped state root for project at {}", project_root.display()))?;
    let actor = resolve_actor(None);
    log_secret_event(&scoped_root, &actor, AuditEventKind::PolicyOverride, "secret_set", key, None);
    Ok(())
}

fn build_store(project_root: &Path) -> Result<Box<dyn SecretStore>> {
    let scoped_root = scoped_state_root(project_root)
        .ok_or_else(|| anyhow!("could not resolve scoped state root for project at {}", project_root.display()))?;
    let scope = resolve_keychain_scope(project_root, &scoped_root);
    Ok(orchestrator_core::build_secret_store(&scope, scoped_root))
}

/// Pick the keychain service-scope string from the adopted scoped state
/// directory name when present, otherwise fall back to the freshly-derived
/// `repo-scope`. This matches the daemon-side behaviour of preferring the
/// *adopted* scope so a moved repo keeps reading the same keychain
/// entries that already match its index file. (codex round-1 P2.)
fn resolve_keychain_scope(project_root: &Path, scoped_root: &Path) -> String {
    if let Some(name) = scoped_root.file_name().and_then(|s| s.to_str()) {
        return name.to_string();
    }
    repository_scope_for_path(project_root)
}

#[derive(Debug, Serialize)]
struct SecretSetOutput {
    ok: bool,
    key: String,
    service: String,
}

fn handle_set(
    args: SecretSetArgs,
    store: &dyn SecretStore,
    scoped_root: &Path,
    actor: &AuditActor,
    cli_json: bool,
) -> Result<()> {
    validate_secret_key(&args.key)?;
    let value = match args.value {
        Some(v) => v,
        None => read_stdin_value()?,
    };
    if value.is_empty() {
        return Err(anyhow!("secret value for KEY {:?} is empty; refusing to store", args.key));
    }
    store.set(&args.key, &value)?;
    log_secret_event(scoped_root, actor, AuditEventKind::PolicyOverride, "secret_set", &args.key, None);
    let json = cli_json || args.json;
    let scope_hint = scope_hint_label(scoped_root);
    if json {
        print_value(
            SecretSetOutput { ok: true, key: args.key.clone(), service: keychain_service_name(&scope_hint) },
            true,
        )?;
    } else {
        println!("stored {} in {} (scope={})", args.key, store.backend_label(), scope_hint);
    }
    Ok(())
}

fn handle_get(args: SecretGetArgs, store: &dyn SecretStore, cli_json: bool) -> Result<()> {
    validate_secret_key(&args.key)?;
    let value =
        store.get(&args.key)?.ok_or_else(|| anyhow!("secret KEY {:?} is not stored for this project", args.key))?;
    let json = cli_json || args.json;
    if json {
        print_value(json!({ "key": args.key, "value": value }), true)?;
    } else {
        if std::io::stdout().is_terminal() {
            eprintln!(
                "warning: printing a stored secret to a terminal; pipe the output (`| pbcopy`, `| xclip`, ...) to avoid leaving the value in shell scrollback"
            );
        }
        println!("{value}");
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct SecretListOutput {
    scope: String,
    service: String,
    keys: Vec<String>,
    index_path: String,
}

fn handle_list(args: SecretListArgs, store: &dyn SecretStore, project_root: &Path, cli_json: bool) -> Result<()> {
    let keys = store.list_keys()?;
    let json = cli_json || args.json;
    let scoped_root = scoped_state_root(project_root)
        .ok_or_else(|| anyhow!("could not resolve scoped state root for project at {}", project_root.display()))?;
    let scope = resolve_keychain_scope(project_root, &scoped_root);
    let service = keychain_service_name(&scope);
    let index_path = secrets_index_path(&scoped_root).display().to_string();
    if json {
        print_value(SecretListOutput { scope, service, keys, index_path }, true)?;
    } else if keys.is_empty() {
        println!("no secrets stored for this project (service={service})");
    } else {
        println!("service: {service}");
        println!("index: {index_path}");
        for key in &keys {
            println!("{key}");
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct SecretRmOutput {
    removed: bool,
    key: String,
}

fn handle_rm(
    args: SecretRmArgs,
    store: &dyn SecretStore,
    scoped_root: &Path,
    actor: &AuditActor,
    cli_json: bool,
) -> Result<()> {
    validate_secret_key(&args.key)?;
    let removed = store.delete(&args.key)?;
    if removed {
        log_secret_event(scoped_root, actor, AuditEventKind::PolicyOverride, "secret_rm", &args.key, None);
    }
    let json = cli_json || args.json;
    if json {
        print_value(SecretRmOutput { removed, key: args.key.clone() }, true)?;
    } else if removed {
        println!("removed {}", args.key);
    } else {
        println!("no stored secret for KEY {}", args.key);
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct SecretImportOutput {
    file: String,
    imported: usize,
    skipped: usize,
    overwritten: usize,
    keys_imported: Vec<String>,
    keys_skipped: Vec<String>,
}

fn handle_import_env(
    args: SecretImportEnvArgs,
    store: &dyn SecretStore,
    project_root: &Path,
    scoped_root: &Path,
    actor: &AuditActor,
    cli_json: bool,
) -> Result<()> {
    let file_path = args.file.map(PathBuf::from).unwrap_or_else(|| project_root.join(".env"));
    let body = fs::read_to_string(&file_path).with_context(|| format!("failed to read {}", file_path.display()))?;
    let entries = parse_dotenv(&body)?;

    let existing: std::collections::BTreeSet<String> = store.list_keys()?.into_iter().collect();
    let mut keys_imported: Vec<String> = Vec::new();
    let mut keys_skipped: Vec<String> = Vec::new();
    let mut overwritten = 0usize;
    // Track keys already written during THIS import so a duplicate
    // entry inside the same `.env` doesn't silently overwrite the
    // first occurrence when `--overwrite` is off. (codex round-10 P3.)
    let mut written_in_this_run: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for (key, value) in entries {
        if let Err(err) = validate_secret_key(&key) {
            eprintln!("warning: skipping {:?}: {}", key, err);
            keys_skipped.push(key);
            continue;
        }
        let already_known = existing.contains(&key) || written_in_this_run.contains(&key);
        if already_known {
            if !args.overwrite {
                keys_skipped.push(key);
                continue;
            }
            overwritten += 1;
        }
        store.set(&key, &value)?;
        written_in_this_run.insert(key.clone());
        keys_imported.push(key);
    }

    log_secret_event(
        scoped_root,
        actor,
        AuditEventKind::PolicyOverride,
        "secret_import_env",
        "*",
        Some(json!({
            "file": file_path.display().to_string(),
            "imported": keys_imported.len(),
            "skipped": keys_skipped.len(),
            "overwritten": overwritten,
        })),
    );

    let json = cli_json || args.json;
    let output = SecretImportOutput {
        file: file_path.display().to_string(),
        imported: keys_imported.len(),
        skipped: keys_skipped.len(),
        overwritten,
        keys_imported,
        keys_skipped,
    };
    if json {
        print_value(output, true)?;
    } else {
        println!(
            "imported {} secret(s) from {} (skipped: {}, overwritten: {})",
            output.imported, output.file, output.skipped, output.overwritten
        );
        if !output.keys_skipped.is_empty() {
            println!("skipped (already present, pass --overwrite to replace): {}", output.keys_skipped.join(", "));
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct SecretExportOutput {
    file: String,
    exported: usize,
}

fn handle_export_env(
    args: SecretExportEnvArgs,
    store: &dyn SecretStore,
    project_root: &Path,
    scoped_root: &Path,
    actor: &AuditActor,
    cli_json: bool,
) -> Result<()> {
    let file_path = args.file.map(PathBuf::from).unwrap_or_else(|| project_root.join(".env.exported"));
    eprintln!(
        "warning: writing plaintext secrets to {} — make sure this path is .gitignore'd and deleted after use",
        file_path.display()
    );
    let mut body = String::new();
    // For export we fail loudly on any per-key keychain read error or
    // missing value — a partial backup is worse than a clear failure.
    // `snapshot_for_spawn` intentionally degrades to "skip" so it can
    // never block plugin spawn; that policy is wrong for `export-env`.
    // (codex round-7 P2.)
    let indexed_keys = store.list_keys()?;
    let snapshot_len = indexed_keys.len();
    for key in &indexed_keys {
        let value = store.get(key)?.ok_or_else(|| {
            anyhow!(
                "secret KEY {:?} is listed in the per-scope index but the keychain returned no value; export aborted to avoid writing a partial backup",
                key
            )
        })?;
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
        body.push_str(&format!("{key}=\"{escaped}\"\n"));
    }
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    }
    write_export_file_owner_only(&file_path, body.as_bytes())
        .with_context(|| format!("failed to write {}", file_path.display()))?;

    log_secret_event(
        scoped_root,
        actor,
        AuditEventKind::PolicyOverride,
        "secret_export_env",
        "*",
        Some(json!({
            "file": file_path.display().to_string(),
            "exported": snapshot_len,
        })),
    );

    let json = cli_json || args.json;
    let output = SecretExportOutput { file: file_path.display().to_string(), exported: snapshot_len };
    if json {
        print_value(output, true)?;
    } else {
        println!("exported {} secret(s) to {}", output.exported, output.file);
    }
    let _ = project_root;
    Ok(())
}

fn read_stdin_value() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .with_context(|| "failed to read secret value from stdin (pass --value or pipe in)")?;
    Ok(buf.trim_end_matches(['\n', '\r']).to_string())
}

/// Write `bytes` to `path` atomically with owner-only (0600) permissions
/// on Unix. The write goes to a sibling temp file that's opened with
/// `mode(0o600)` and then renamed over the target — so even if `path`
/// already existed with looser bits, the plaintext secret is never
/// visible to other local users between truncate and chmod. On Windows
/// the umask isn't applicable; the call falls back to `fs::write`.
/// (codex round-2 P2 + round-3 P2.)
fn write_export_file_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "export path has no file name"))?;
        // Use `create_new` so an attacker pre-creating the predictable
        // temp path (regular file or symlink) cannot have us write
        // plaintext into it with looser bits. Retry on collision with a
        // random suffix until we win. (codex round-4 P2.)
        let mut attempts = 0u32;
        let mut tmp_path;
        loop {
            let suffix: u64 = uuid::Uuid::new_v4().as_u128() as u64;
            tmp_path = parent.join(format!(".{file_name}.animus-secret-tmp-{suffix:x}"));
            match fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&tmp_path) {
                Ok(mut f) => {
                    std::io::Write::write_all(&mut f, bytes)?;
                    f.sync_all()?;
                    break;
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && attempts < 8 => {
                    attempts += 1;
                    continue;
                }
                Err(err) => return Err(err),
            }
        }
        fs::rename(&tmp_path, path)?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, bytes)?;
    }
    Ok(())
}

fn scope_hint_label(scoped_root: &Path) -> String {
    scoped_root.file_name().and_then(|s| s.to_str()).map(str::to_string).unwrap_or_else(|| "unknown".to_string())
}

fn log_secret_event(
    scoped_root: &Path,
    actor: &AuditActor,
    kind: AuditEventKind,
    event_label: &str,
    key: &str,
    extra: Option<serde_json::Value>,
) {
    let mut details = json!({
        "event": event_label,
        "key": key,
    });
    if let Some(extra) = extra {
        if let (Some(obj), Some(extra_obj)) = (details.as_object_mut(), extra.as_object()) {
            for (k, v) in extra_obj {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    Audit::at_scoped_root(scoped_root).log(actor.clone(), kind, details);
}

fn resolve_actor(as_principal: Option<&str>) -> AuditActor {
    let path = default_principals_path();
    let _ = bootstrap_principals_file_if_absent(&path);
    let file = load_principals_file(&path).ok().flatten().unwrap_or_default();
    let os_user = current_os_username();
    let entry = match as_principal {
        Some(id) => resolve_principal_by_id(&file, id),
        None => os_user.as_deref().and_then(|u| resolve_principal_for_os_user(&file, u)),
    };
    match entry {
        Some(entry) => {
            let kind = match entry.kind {
                PrincipalKind::User => "user",
                PrincipalKind::Service => "service_account",
            };
            AuditActor::Principal { id: entry.id.clone(), kind }
        }
        None => {
            let id = os_user.clone().unwrap_or_else(|| "unknown".to_string());
            AuditActor::Principal { id, kind: "user" }
        }
    }
}

/// Decode a double-quoted dotenv value body (the bytes between the
/// surrounding `"` characters) into the original string. Recognises
/// `\\` (literal backslash), `\"` (literal double quote), and `\n`
/// (newline). Doing this with a single left-to-right scan avoids the
/// `replace().replace()` ordering bug where decoding `\n` before
/// `\\` could turn an escaped backslash followed by `n` into a real
/// newline. (codex round-7 P2.)
fn decode_double_quoted(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Minimal `.env` parser. Supports `KEY=VALUE`, `KEY="quoted value"`,
/// `KEY='single-quoted'`, blank lines, and `# comments`. Trims
/// surrounding whitespace on both sides of `=`.
fn parse_dotenv(body: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for (lineno, raw) in body.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let stripped = line.strip_prefix("export ").unwrap_or(line);
        let Some(eq) = stripped.find('=') else {
            return Err(anyhow!(".env line {}: no `=` found: {raw:?}", lineno + 1));
        };
        let key = stripped[..eq].trim().to_string();
        let raw_value = stripped[eq + 1..].trim();
        let value = if let Some(rest) = raw_value.strip_prefix('"') {
            decode_double_quoted(rest.strip_suffix('"').unwrap_or(rest))
        } else if let Some(rest) = raw_value.strip_prefix('\'') {
            rest.strip_suffix('\'').unwrap_or(rest).to_string()
        } else {
            raw_value.to_string()
        };
        out.push((key, value));
    }
    Ok(out)
}

#[allow(unused_imports)]
use std::collections::BTreeMap as _BTreeMap;

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::MockSecretStore;

    #[test]
    fn parse_dotenv_handles_quoted_and_unquoted_and_comments() {
        let body = "\
# a comment
KEY1=hello
KEY2=\"world with spaces\"
KEY3='single-quoted'
export KEY4=also-supported

EMPTY=
";
        let entries = parse_dotenv(body).unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0], ("KEY1".to_string(), "hello".to_string()));
        assert_eq!(entries[1], ("KEY2".to_string(), "world with spaces".to_string()));
        assert_eq!(entries[2], ("KEY3".to_string(), "single-quoted".to_string()));
        assert_eq!(entries[3], ("KEY4".to_string(), "also-supported".to_string()));
        assert_eq!(entries[4], ("EMPTY".to_string(), "".to_string()));
    }

    #[test]
    fn parse_dotenv_rejects_lines_without_eq() {
        let err = parse_dotenv("KEY1\n").unwrap_err();
        assert!(format!("{err}").contains("no `=` found"));
    }

    #[test]
    fn decode_double_quoted_preserves_literal_backslash_n() {
        // A value containing the literal two characters `\n` (backslash
        // + 'n') is exported as `\\n`. The decoder must reproduce the
        // original literal, not an actual newline. (codex round-7 P2.)
        assert_eq!(decode_double_quoted(r"\\n"), r"\n");
        assert_eq!(decode_double_quoted(r"\n"), "\n");
        assert_eq!(decode_double_quoted(r#"\""#), "\"");
        assert_eq!(decode_double_quoted(r"\\"), r"\");
        assert_eq!(decode_double_quoted(r"a\\nb\nc"), "a\\nb\nc");
    }

    #[test]
    fn mock_store_round_trip_via_ops_surface() {
        let store = MockSecretStore::new();
        store.set("LINEAR_API_TOKEN", "sk-test").unwrap();
        store.set("OPENAI_API_KEY", "sk-other").unwrap();
        assert_eq!(store.get("LINEAR_API_TOKEN").unwrap().as_deref(), Some("sk-test"));
        let mut keys = store.list_keys().unwrap();
        keys.sort();
        assert_eq!(keys, vec!["LINEAR_API_TOKEN", "OPENAI_API_KEY"]);
        assert!(store.delete("LINEAR_API_TOKEN").unwrap());
    }
}
