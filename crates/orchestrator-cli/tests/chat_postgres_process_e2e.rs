//! Real-process shared chat authority acceptance gate.
//!
//! This test is intentionally opt-in because it needs a reachable PostgreSQL
//! database and a packaged `animus-postgres` executable. Enable it with:
//!
//! ```text
//! ANIMUS_CHAT_POSTGRES_PROCESS_E2E=1 \
//! ANIMUS_CHAT_POSTGRES_PROCESS_E2E_DATABASE_URL=postgres://... \
//! ANIMUS_CHAT_POSTGRES_PROCESS_E2E_PLUGIN_BIN=/path/to/animus-postgres \
//! cargo test -p orchestrator-cli --test chat_postgres_process_e2e -- --nocapture
//! ```
//!
//! When enabled, missing or invalid dependencies fail the test. Each CLI
//! invocation gets a distinct HOME and plugin registry while retaining one
//! canonical project root and PostgreSQL database. Therefore host-local locks,
//! SQLite journals, and process memory cannot make the assertions pass.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use postgres::{Client, NoTls};
use serde_json::{json, Value};
use tempfile::TempDir;
use uuid::Uuid;

const ENABLE_ENV: &str = "ANIMUS_CHAT_POSTGRES_PROCESS_E2E";
const DATABASE_URL_ENV: &str = "ANIMUS_CHAT_POSTGRES_PROCESS_E2E_DATABASE_URL";
const PLUGIN_BIN_ENV: &str = "ANIMUS_CHAT_POSTGRES_PROCESS_E2E_PLUGIN_BIN";
const PROVIDER_COUNTER: &str = ".animus-provider-mock-executions";
const PROVIDER_RELEASE: &str = ".animus-provider-mock-release";
const PROVIDER_MARKER: &str = "ANIMUS_CHAT_POSTGRES_PROCESS_E2E";
const ACTOR_ID: &str = "task1005-process-user";
const TENANT_ID: &str = "task1005-process-tenant";

#[derive(Debug)]
struct GateConfig {
    database_url: String,
    postgres_plugin: PathBuf,
}

fn gate_config() -> Option<GateConfig> {
    let Some(enabled) = std::env::var_os(ENABLE_ENV) else {
        eprintln!("skipping process-level PostgreSQL chat gate; set {ENABLE_ENV}=1 to enable");
        return None;
    };
    let enabled = enabled.to_string_lossy();
    assert!(matches!(enabled.as_ref(), "1" | "true"), "{ENABLE_ENV} must be exactly 1 or true when set");

    let database_url = std::env::var(DATABASE_URL_ENV)
        .unwrap_or_else(|_| panic!("{DATABASE_URL_ENV} is required when {ENABLE_ENV}=1"));
    assert!(!database_url.trim().is_empty(), "{DATABASE_URL_ENV} must not be empty");
    let postgres_plugin = PathBuf::from(
        std::env::var_os(PLUGIN_BIN_ENV).unwrap_or_else(|| panic!("{PLUGIN_BIN_ENV} is required when {ENABLE_ENV}=1")),
    );
    assert!(postgres_plugin.is_file(), "PostgreSQL plugin does not exist: {}", postgres_plugin.display());

    Some(GateConfig { database_url, postgres_plugin })
}

struct Harness {
    _root: TempDir,
    project: PathBuf,
    plugin_dir: PathBuf,
    cli: PathBuf,
    database_url: String,
    path: OsString,
}

impl Harness {
    fn new(config: &GateConfig) -> Self {
        let root = tempfile::tempdir().expect("create process E2E temp root");
        let project = root.path().join("project");
        let plugin_dir = root.path().join("plugins");
        fs::create_dir_all(&project).expect("create process E2E project");
        fs::create_dir_all(&plugin_dir).expect("create isolated plugin directory");

        let cli = PathBuf::from(env!("CARGO_BIN_EXE_animus"));
        let provider = PathBuf::from(env!("CARGO_BIN_EXE_animus-provider-mock"));
        assert!(cli.is_file(), "Cargo-provided animus binary is missing: {}", cli.display());
        assert!(provider.is_file(), "Cargo-provided mock provider is missing: {}", provider.display());

        // ANIMUS_PLUGIN_PATH intentionally scans only admitted plugin/provider
        // filename prefixes. The release artifact's manifest remains
        // `animus-postgres`; this discoverable alias mirrors installation.
        let staged_postgres = plugin_dir.join("animus-plugin-postgres");
        let staged_provider = plugin_dir.join("animus-provider-mock");
        fs::copy(&config.postgres_plugin, &staged_postgres).expect("stage packaged animus-postgres executable");
        fs::copy(&provider, &staged_provider).expect("stage Cargo-built mock provider executable");

        let manifest = Command::new(&staged_postgres)
            .arg("--manifest")
            .output()
            .expect("execute staged animus-postgres --manifest");
        assert_process_success("animus-postgres --manifest", &manifest);
        let manifest: Value = serde_json::from_slice(&manifest.stdout).expect("parse animus-postgres manifest JSON");
        assert_eq!(manifest.get("name").and_then(Value::as_str), Some("animus-postgres"));
        let capabilities =
            manifest.get("capabilities").and_then(Value::as_array).expect("animus-postgres manifest capabilities");
        for capability in [
            "conversation_operations_shared_v1",
            "conversation_operation_fenced_append_v1",
            "conversation/operation_begin",
            "conversation/operation_terminalize",
        ] {
            assert!(
                capabilities.iter().any(|candidate| candidate.as_str() == Some(capability)),
                "packaged animus-postgres is missing {capability}"
            );
        }

        let path = std::env::var_os("PATH").expect("process E2E requires PATH for the Node plugin shebang");
        Self { _root: root, project, plugin_dir, cli, database_url: config.database_url.clone(), path }
    }

    fn command(&self, runtime: &str) -> Command {
        let home = self._root.path().join(format!("home-{runtime}"));
        let config = home.join("config");
        let installed_plugins = home.join("installed-plugins");
        fs::create_dir_all(&config).expect("create isolated runtime config directory");
        fs::create_dir_all(&installed_plugins).expect("create isolated runtime plugin directory");

        let mut command = Command::new(&self.cli);
        command
            .env_clear()
            .env("PATH", &self.path)
            .env("HOME", &home)
            .env("ANIMUS_CONFIG_DIR", &config)
            .env("ANIMUS_PLUGIN_DIR", &installed_plugins)
            .env("ANIMUS_PLUGIN_PATH", &self.plugin_dir)
            .env("BASE_DB_URL", &self.database_url)
            .env("ANIMUS_POSTGRES_AUTO_MIGRATE", "1")
            .env("NO_COLOR", "1")
            .arg("--json")
            .arg("--project-root")
            .arg(&self.project);
        command
    }

    fn actor_json(&self) -> String {
        json!({ "user_id": ACTOR_ID, "tenant_id": TENANT_ID, "claims": [] }).to_string()
    }

    fn assert_shared_backend_ready(&self) {
        let output =
            self.command("capability-probe").args(["chat", "capabilities"]).output().expect("run chat capabilities");
        assert_process_success("chat capabilities", &output);
        let payload: Value = serde_json::from_slice(&output.stdout).expect("parse chat capabilities JSON envelope");
        assert_eq!(payload.pointer("/data/backend/kind").and_then(Value::as_str), Some("plugin"), "{payload:#}");
        assert_eq!(
            payload.pointer("/data/backend/authority_mode").and_then(Value::as_str),
            Some("shared_conversation_store_rpc"),
            "{payload:#}"
        );
        assert_eq!(
            payload.pointer("/data/backend/required_capabilities_observed").and_then(Value::as_bool),
            Some(true),
            "{payload:#}"
        );
        assert_eq!(payload.pointer("/data/backend/ready").and_then(Value::as_bool), Some(true), "{payload:#}");
        assert_eq!(payload.pointer("/data/backend/error_code"), Some(&Value::Null), "{payload:#}");
    }

    fn create_conversation(&self, id: &str, runtime: &str) {
        let actor = self.actor_json();
        let output = self
            .command(runtime)
            .args([
                "chat",
                "new",
                "--id",
                id,
                "--title",
                "TASK-1005 process acceptance",
                "--as-user",
                ACTOR_ID,
                "--actor-json",
                &actor,
            ])
            .output()
            .expect("run chat new");
        assert_process_success("chat new", &output);
    }

    fn send_command(&self, runtime: &str, conversation: &str, key: &str, message: &str) -> Command {
        let actor = self.actor_json();
        let mut command = self.command(runtime);
        command.args([
            "chat",
            "send",
            message,
            "--conversation",
            conversation,
            "--tool",
            "mock",
            "--model",
            "mock-fast-1",
            "--as-user",
            ACTOR_ID,
            "--actor-json",
            &actor,
            "--idempotency-key",
            key,
            "--require-shared-authority",
            "--no-animus-mcp",
        ]);
        command
    }

    fn provider_execution_count(&self) -> usize {
        let counter = self.project.join(PROVIDER_COUNTER);
        fs::read_to_string(counter).map(|text| text.lines().count()).unwrap_or(0)
    }

    fn hold_provider(&self) {
        let release = self.project.join(PROVIDER_RELEASE);
        match fs::remove_file(release) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("reset provider release latch: {error}"),
        }
    }

    fn release_provider(&self) {
        fs::write(self.project.join(PROVIDER_RELEASE), b"release\n").expect("release provider execution");
    }

    fn wait_for_provider_execution(&self, expected: usize, child: &mut Child) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if self.provider_execution_count() >= expected {
                return;
            }
            if let Some(status) = child.try_wait().expect("inspect leading CLI process") {
                panic!("leading CLI exited before provider execution {expected}: {status}");
            }
            assert!(Instant::now() < deadline, "timed out waiting for provider execution {expected}");
            thread::sleep(Duration::from_millis(25));
        }
    }
}

fn process_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_process_success(label: &str, output: &Output) {
    assert!(output.status.success(), "{label} failed ({})\n{}", output.status, process_text(output));
}

fn wait_for_child(mut child: Child, label: &str) -> Output {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait().expect("poll CLI subprocess") {
            Some(_) => return child.wait_with_output().expect("collect CLI subprocess output"),
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            None => {
                child.kill().expect("kill hung CLI subprocess");
                let output = child.wait_with_output().expect("collect killed CLI subprocess output");
                panic!("{label} timed out\n{}", process_text(&output));
            }
        }
    }
}

fn spawn(command: &mut Command, label: &str) -> Child {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {label}: {error}"))
}

#[derive(Debug)]
struct ConversationEvidence {
    conversation_count: i64,
    message_count: i64,
    user_count: i64,
    assistant_count: i64,
    metadata_message_count: i64,
    active_operation_id: Option<String>,
    operation_state: String,
    operation_user_seq: Option<i64>,
    operation_assistant_seq: Option<i64>,
    execution_hash: Option<String>,
    lease_token: Option<String>,
    lease_expires_at: Option<i64>,
}

fn evidence(client: &mut Client, conversation: &str, key: &str) -> ConversationEvidence {
    let conversation_row = client
        .query_one(
            "SELECT COUNT(*)::bigint, COALESCE(MAX(message_count), -1)::bigint, MAX(active_operation_id) \
             FROM chat_conversation WHERE tenant_id = $1 AND id = $2",
            &[&TENANT_ID, &conversation],
        )
        .expect("query canonical conversation row");
    let message_row = client
        .query_one(
            "SELECT COUNT(*)::bigint, \
                    COUNT(*) FILTER (WHERE role = 'user')::bigint, \
                    COUNT(*) FILTER (WHERE role = 'assistant')::bigint \
             FROM chat_message WHERE tenant_id = $1 AND conversation_id = $2",
            &[&TENANT_ID, &conversation],
        )
        .expect("query canonical transcript rows");
    let operation_row = client
        .query_one(
            "SELECT state, user_seq, assistant_seq, execution_hash, lease_token, lease_expires_at \
             FROM chat_operation \
             WHERE tenant_id = $1 AND conversation_id = $2 AND actor_id = $3 AND caller_key = $4",
            &[&TENANT_ID, &conversation, &ACTOR_ID, &key],
        )
        .expect("query canonical operation row");

    ConversationEvidence {
        conversation_count: conversation_row.get(0),
        metadata_message_count: conversation_row.get(1),
        active_operation_id: conversation_row.get(2),
        message_count: message_row.get(0),
        user_count: message_row.get(1),
        assistant_count: message_row.get(2),
        operation_state: operation_row.get(0),
        operation_user_seq: operation_row.get(1),
        operation_assistant_seq: operation_row.get(2),
        execution_hash: operation_row.get(3),
        lease_token: operation_row.get(4),
        lease_expires_at: operation_row.get(5),
    }
}

fn assert_common_terminal_evidence(value: &ConversationEvidence) {
    assert_eq!(value.conversation_count, 1, "one canonical conversation row: {value:?}");
    assert_eq!(value.user_count, 1, "one canonical user row: {value:?}");
    assert_eq!(value.operation_user_seq, Some(0), "operation must bind canonical user seq: {value:?}");
    assert!(value.execution_hash.is_some(), "provider execution must be bound before it starts: {value:?}");
    assert!(value.active_operation_id.is_none(), "terminal operation must clear reservation: {value:?}");
    assert!(value.lease_token.is_none(), "terminal operation must clear lease token: {value:?}");
    assert!(value.lease_expires_at.is_none(), "terminal operation must clear lease expiry: {value:?}");
}

fn delete_conversation(client: &mut Client, conversation: &str) {
    client
        .execute("DELETE FROM chat_conversation WHERE tenant_id = $1 AND id = $2", &[&TENANT_ID, &conversation])
        .expect("clean process E2E conversation");
}

#[test]
fn two_cli_processes_share_postgres_operation_authority_and_fence_stale_assistants() {
    let Some(config) = gate_config() else { return };
    let harness = Harness::new(&config);
    let mut database = Client::connect(&config.database_url, NoTls)
        .unwrap_or_else(|error| panic!("{DATABASE_URL_ENV} is not a reachable PostgreSQL database: {error}"));
    let version: String = database.query_one("SHOW server_version", &[]).expect("query PostgreSQL version").get(0);
    eprintln!("running shared-authority process gate against PostgreSQL {version}");
    harness.assert_shared_backend_ready();

    // Successful overlap: runtime A owns the provider execution and remains
    // held until runtime B observes the shared in-progress admission. A third
    // fresh process proves terminal replay never invokes the provider again.
    let successful_conversation = format!("task1005-success-{}", Uuid::new_v4());
    let successful_key = format!("task1005-success-{}", Uuid::new_v4());
    let successful_message = format!("{PROVIDER_MARKER}: produce one canonical answer");
    harness.create_conversation(&successful_conversation, "success-setup");
    harness.hold_provider();

    let mut leader_command =
        harness.send_command("success-leader", &successful_conversation, &successful_key, &successful_message);
    let mut leader = spawn(&mut leader_command, "successful leader");
    harness.wait_for_provider_execution(1, &mut leader);

    let follower = harness
        .send_command("success-follower", &successful_conversation, &successful_key, &successful_message)
        .output()
        .expect("run concurrent successful follower");
    assert!(!follower.status.success(), "concurrent follower cannot complete while the provider is held");
    assert!(
        process_text(&follower).contains("idempotency_in_progress"),
        "concurrent follower must observe the shared in-progress admission\n{}",
        process_text(&follower)
    );
    harness.release_provider();
    let leader = wait_for_child(leader, "successful leader");
    assert_process_success("successful leader", &leader);

    let replay = harness
        .send_command("success-replay", &successful_conversation, &successful_key, &successful_message)
        .output()
        .expect("run completed-operation replay");
    assert_process_success("completed-operation replay", &replay);
    assert_eq!(harness.provider_execution_count(), 1, "overlap and replay must execute the provider at most once");

    let successful = evidence(&mut database, &successful_conversation, &successful_key);
    assert_common_terminal_evidence(&successful);
    assert_eq!(successful.message_count, 2, "successful transcript must contain one turn: {successful:?}");
    assert_eq!(successful.metadata_message_count, 2, "successful metadata count must be canonical: {successful:?}");
    assert_eq!(successful.assistant_count, 1, "successful transcript must contain one assistant: {successful:?}");
    assert_eq!(successful.operation_state, "completed", "successful operation must be terminal: {successful:?}");
    assert_eq!(successful.operation_assistant_seq, Some(1), "receipt must bind canonical assistant: {successful:?}");

    // Lease handoff while the original provider is still running: runtime B
    // recovers the exact accepted operation and records interruption. Runtime
    // A's later assistant append carries its stale lease fence and must be
    // rejected by animus-postgres. A third process replays that terminal result
    // without invoking the provider.
    let fenced_conversation = format!("task1005-fence-{}", Uuid::new_v4());
    let fenced_key = format!("task1005-fence-{}", Uuid::new_v4());
    let fenced_message = format!("{PROVIDER_MARKER}: this stale answer must be fenced");
    harness.create_conversation(&fenced_conversation, "fence-setup");
    harness.hold_provider();

    let mut stale_command = harness.send_command("fence-stale", &fenced_conversation, &fenced_key, &fenced_message);
    let mut stale = spawn(&mut stale_command, "stale lease holder");
    harness.wait_for_provider_execution(2, &mut stale);

    let expired = database
        .execute(
            "UPDATE chat_operation \
             SET lease_expires_at = FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::bigint - 1 \
             WHERE tenant_id = $1 AND conversation_id = $2 AND actor_id = $3 AND caller_key = $4 \
               AND state = 'user_accepted' AND lease_token IS NOT NULL",
            &[&TENANT_ID, &fenced_conversation, &ACTOR_ID, &fenced_key],
        )
        .expect("expire original provider lease");
    assert_eq!(expired, 1, "provider must have one accepted lease to expire");

    let recovery = harness
        .send_command("fence-recovery", &fenced_conversation, &fenced_key, &fenced_message)
        .output()
        .expect("run recovered operation reconciliation");
    assert!(!recovery.status.success(), "recovery must replay terminal interruption\n{}", process_text(&recovery));
    assert!(
        process_text(&recovery).contains("assistant_interrupted"),
        "recovery must terminalize the accepted user without repeating provider execution\n{}",
        process_text(&recovery)
    );

    harness.release_provider();
    let stale = wait_for_child(stale, "stale lease holder");
    assert!(!stale.status.success(), "stale lease holder must not commit an assistant\n{}", process_text(&stale));

    let interrupted_replay = harness
        .send_command("fence-replay", &fenced_conversation, &fenced_key, &fenced_message)
        .output()
        .expect("run interrupted-operation replay");
    assert!(!interrupted_replay.status.success(), "interrupted replay must preserve terminal failure");
    assert!(process_text(&interrupted_replay).contains("assistant_interrupted"));
    assert_eq!(
        harness.provider_execution_count(),
        2,
        "fenced handoff and terminal replay must not start a second provider for the operation"
    );

    let fenced = evidence(&mut database, &fenced_conversation, &fenced_key);
    assert_common_terminal_evidence(&fenced);
    assert_eq!(fenced.message_count, 1, "fenced transcript must retain only its canonical user: {fenced:?}");
    assert_eq!(fenced.metadata_message_count, 1, "fenced metadata count must match durable rows: {fenced:?}");
    assert_eq!(fenced.assistant_count, 0, "stale provider output must never create an assistant: {fenced:?}");
    assert_eq!(
        fenced.operation_state, "assistant_interrupted",
        "recovered lease must terminalize the stale execution: {fenced:?}"
    );
    assert_eq!(fenced.operation_assistant_seq, None, "fenced operation cannot claim an assistant: {fenced:?}");

    delete_conversation(&mut database, &successful_conversation);
    delete_conversation(&mut database, &fenced_conversation);
}
