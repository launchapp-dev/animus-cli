use super::runner::{PluginInstaller, PluginPreflightRunner};
use super::{InstalledPluginSummary, PluginPreflightSpec, RequiredRole};
use anyhow::Result;
use async_trait::async_trait;
use std::cell::RefCell;

struct FakeInstaller {
    install_calls: RefCell<Vec<String>>,
    next_after_install: RefCell<Vec<InstalledPluginSummary>>,
}

impl FakeInstaller {
    fn new(initial_after_install: Vec<InstalledPluginSummary>) -> Self {
        Self { install_calls: RefCell::new(Vec::new()), next_after_install: RefCell::new(initial_after_install) }
    }
}

#[async_trait(?Send)]
impl PluginInstaller for FakeInstaller {
    async fn install(&self, repo_spec: &str) -> Result<String> {
        self.install_calls.borrow_mut().push(repo_spec.to_string());
        Ok(repo_spec.to_string())
    }

    async fn rediscover(&self) -> Result<Vec<InstalledPluginSummary>> {
        Ok(self.next_after_install.borrow().clone())
    }
}

fn provider_plugin(name: &str) -> InstalledPluginSummary {
    InstalledPluginSummary { name: name.to_string(), plugin_kind: "provider".to_string(), subject_kinds: Vec::new() }
}

fn subject_plugin(name: &str, kinds: &[&str]) -> InstalledPluginSummary {
    InstalledPluginSummary {
        name: name.to_string(),
        plugin_kind: "subject_backend".to_string(),
        subject_kinds: kinds.iter().map(|k| (*k).to_string()).collect(),
    }
}

fn workflow_runner_plugin(name: &str) -> InstalledPluginSummary {
    InstalledPluginSummary {
        name: name.to_string(),
        plugin_kind: "workflow_runner".to_string(),
        subject_kinds: Vec::new(),
    }
}

fn queue_plugin(name: &str) -> InstalledPluginSummary {
    InstalledPluginSummary { name: name.to_string(), plugin_kind: "queue".to_string(), subject_kinds: Vec::new() }
}

#[tokio::test]
async fn preflight_with_no_plugins_and_no_auto_install_reports_missing_with_fix_commands() {
    let spec = PluginPreflightSpec::daemon_default();
    let result = PluginPreflightRunner::run(&spec, Vec::new(), None).await.expect("preflight run");

    assert!(!result.is_ok(), "preflight should fail when no plugins installed");
    assert_eq!(result.missing.len(), 5, "all five roles should be missing");

    let provider_missing = result.missing.iter().find(|m| m.role == "at_least_one_provider").expect("provider role");
    assert!(provider_missing.fix_command.contains("animus plugin install"));
    assert!(provider_missing.fix_command.contains("animus-provider-claude"));

    let task_missing = result.missing.iter().find(|m| m.role == "subject_kind:task").expect("task subject role");
    assert!(task_missing.fix_command.contains("subject_kind:task"));

    let message = result.render_missing_message();
    assert!(message.contains("plugin preflight failed"));
    assert!(message.contains("at_least_one_provider"));
    assert!(message.contains("--auto-install"));
}

#[tokio::test]
async fn preflight_with_provider_installed_marks_provider_role_satisfied() {
    let spec = PluginPreflightSpec::daemon_default();
    let installed = vec![provider_plugin("animus-provider-claude")];
    let result = PluginPreflightRunner::run(&spec, installed, None).await.expect("preflight run");

    assert!(result.satisfied.contains(&"at_least_one_provider".to_string()));
    assert!(!result.is_ok(), "subject backends still missing");
    let missing_labels: Vec<&str> = result.missing.iter().map(|m| m.role.as_str()).collect();
    assert!(missing_labels.contains(&"subject_kind:task"));
    assert!(missing_labels.contains(&"subject_kind:requirement"));
}

#[tokio::test]
async fn preflight_with_auto_install_installs_missing_plugin_and_marks_satisfied() {
    let spec = PluginPreflightSpec {
        required_roles: vec![RequiredRole::AtLeastOneProvider],
        auto_install: true,
        auto_install_defaults: vec![(
            "at_least_one_provider".to_string(),
            "launchapp-dev/animus-provider-claude@v0.1.0".to_string(),
        )],
    };
    let installer = FakeInstaller::new(vec![provider_plugin("animus-provider-claude")]);

    let result = PluginPreflightRunner::run(&spec, Vec::new(), Some(&installer)).await.expect("preflight run");

    assert!(result.is_ok(), "auto-install should resolve missing role");
    assert_eq!(installer.install_calls.borrow().len(), 1);
    assert_eq!(installer.install_calls.borrow()[0], "launchapp-dev/animus-provider-claude@v0.1.0");
    assert_eq!(result.auto_installed.len(), 1);
    assert_eq!(result.auto_installed[0].role, "at_least_one_provider");
}

#[tokio::test]
async fn preflight_with_auto_install_but_install_still_does_not_cover_role_reports_missing() {
    let spec = PluginPreflightSpec {
        required_roles: vec![RequiredRole::SubjectKind("task".to_string())],
        auto_install: true,
        auto_install_defaults: vec![(
            "subject_kind:task".to_string(),
            "launchapp-dev/animus-subject-broken@v0.1.0".to_string(),
        )],
    };
    let installer = FakeInstaller::new(vec![subject_plugin("animus-subject-broken", &["unrelated"])]);

    let result = PluginPreflightRunner::run(&spec, Vec::new(), Some(&installer)).await.expect("preflight run");

    assert!(!result.is_ok(), "preflight should still fail when installed plugin doesn't claim the kind");
    assert_eq!(result.missing.len(), 1);
    assert!(result.missing[0].fix_command.contains("auto-install ran"));
}

#[tokio::test]
async fn preflight_satisfied_when_subject_backend_covers_all_required_kinds() {
    let spec = PluginPreflightSpec::daemon_default();
    let installed = vec![
        provider_plugin("animus-provider-claude"),
        subject_plugin("animus-subject-native", &["task", "requirement"]),
        workflow_runner_plugin("animus-workflow-runner-default"),
        queue_plugin("animus-queue-default"),
    ];
    let result = PluginPreflightRunner::run(&spec, installed, None).await.expect("preflight run");

    assert!(result.is_ok(), "all roles satisfied");
    assert_eq!(result.missing.len(), 0);
    assert_eq!(result.satisfied.len(), 5);
}

#[tokio::test]
async fn preflight_refuses_when_workflow_runner_or_queue_plugin_missing() {
    let spec = PluginPreflightSpec::daemon_default();
    // Subjects + provider present; workflow_runner + queue missing.
    let installed = vec![
        provider_plugin("animus-provider-claude"),
        subject_plugin("animus-subject-native", &["task", "requirement"]),
    ];
    let result = PluginPreflightRunner::run(&spec, installed, None).await.expect("preflight run");

    assert!(!result.is_ok(), "preflight must refuse-to-start when workflow_runner or queue missing");
    let missing_labels: Vec<&str> = result.missing.iter().map(|m| m.role.as_str()).collect();
    assert!(missing_labels.contains(&"workflow_runner"));
    assert!(missing_labels.contains(&"queue"));

    let wf = result.missing.iter().find(|m| m.role == "workflow_runner").unwrap();
    assert!(
        wf.fix_command.contains("animus-workflow-runner-default"),
        "workflow_runner fix command should point at the curated default plugin, got: {}",
        wf.fix_command
    );
    let q = result.missing.iter().find(|m| m.role == "queue").unwrap();
    assert!(
        q.fix_command.contains("animus-queue-default"),
        "queue fix command should point at the curated default plugin, got: {}",
        q.fix_command
    );
}

#[test]
fn install_target_for_resolves_workflow_runner_and_queue_roles() {
    let spec = PluginPreflightSpec::daemon_default();
    let wf = spec.install_target_for("workflow_runner").expect("workflow_runner role mapped");
    assert!(
        wf.starts_with("launchapp-dev/animus-workflow-runner-default@"),
        "workflow_runner role must map to the curated default workflow-runner plugin, got: {wf}"
    );
    let q = spec.install_target_for("queue").expect("queue role mapped");
    assert!(
        q.starts_with("launchapp-dev/animus-queue-default@"),
        "queue role must map to the curated default queue plugin, got: {q}"
    );
}

// Codex round-4 P2: curated provider repos (launchapp-dev/animus-provider-*)
// claim reserved tool names (claude / codex / ...). The bare install command
// is rejected by enforce_provider_tool_policy. The preflight fix string MUST
// include --allow-shadow-builtin so following the printed advice actually
// succeeds.
#[tokio::test]
async fn provider_missing_preflight_suggests_command_that_actually_works() {
    let spec = PluginPreflightSpec::daemon_default();
    let result = PluginPreflightRunner::run(&spec, Vec::new(), None).await.expect("preflight run");

    let provider_missing =
        result.missing.iter().find(|m| m.role == "at_least_one_provider").expect("provider role missing");
    assert!(
        provider_missing.fix_command.contains("--allow-shadow-builtin"),
        "fix command MUST include --allow-shadow-builtin (curated provider repos shadow built-in tools), \
         got: {}",
        provider_missing.fix_command
    );
    assert!(
        provider_missing.fix_command.contains("animus plugin install"),
        "fix command must still be an install invocation: {}",
        provider_missing.fix_command
    );

    let subject_missing =
        result.missing.iter().find(|m| m.role.starts_with("subject_kind:")).expect("subject role missing");
    assert!(
        subject_missing.fix_command.contains("--allow-shadow-builtin"),
        "subject fix command also includes --allow-shadow-builtin so a curated subject backend install \
         that shadows a built-in adapter is accepted: {}",
        subject_missing.fix_command
    );
}

#[tokio::test]
async fn auto_install_failure_fix_command_includes_allow_shadow_builtin() {
    let spec = PluginPreflightSpec {
        required_roles: vec![RequiredRole::SubjectKind("task".to_string())],
        auto_install: true,
        auto_install_defaults: vec![(
            "subject_kind:task".to_string(),
            "launchapp-dev/animus-subject-broken@v0.1.0".to_string(),
        )],
    };
    let installer = FakeInstaller::new(vec![subject_plugin("animus-subject-broken", &["unrelated"])]);

    let result = PluginPreflightRunner::run(&spec, Vec::new(), Some(&installer)).await.expect("preflight run");

    assert_eq!(result.missing.len(), 1);
    assert!(
        result.missing[0].fix_command.contains("--allow-shadow-builtin"),
        "post-auto-install fix command MUST also advertise --allow-shadow-builtin: {}",
        result.missing[0].fix_command
    );
}

#[test]
fn install_target_for_resolves_role_labels_to_repo_specs() {
    let spec = PluginPreflightSpec::daemon_default();
    let provider = spec.install_target_for("at_least_one_provider").expect("provider role mapped");
    assert!(
        provider.starts_with("launchapp-dev/animus-provider-claude@"),
        "provider role must map to the curated claude provider, got: {provider}"
    );
    let task = spec.install_target_for("subject_kind:task").expect("task role mapped");
    assert!(
        task.starts_with("launchapp-dev/animus-subject-default@"),
        "task role must map to animus-subject-default (NOT animus-subject-linear), got: {task}"
    );
    let requirement = spec.install_target_for("subject_kind:requirement").expect("requirement role mapped");
    assert!(
        requirement.starts_with("launchapp-dev/animus-subject-requirements@"),
        "requirement role must map to animus-subject-requirements (NOT animus-subject-linear), got: {requirement}"
    );
    assert_eq!(spec.install_target_for("nonexistent_role"), None);
}
