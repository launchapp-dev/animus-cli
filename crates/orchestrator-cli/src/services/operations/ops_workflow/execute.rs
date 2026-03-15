use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use orchestrator_core::services::ServiceHub;

use crate::services::runtime::execution_fact_projection::project_terminal_workflow_result;
use crate::{print_value, WorkflowExecuteArgs};
use ::workflow_runner_v2::workflow_execute::{execute_workflow, PhaseEvent, WorkflowExecuteParams};

pub(crate) async fn handle_workflow_execute(
    mut args: WorkflowExecuteArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    if args.requirement_id.is_some() && args.workflow_ref.is_none() {
        args.workflow_ref = Some(super::resolve_requirement_workflow_ref(project_root)?);
    }
    if args.workflow_id.is_some() && !args.vars.is_empty() {
        anyhow::bail!(
            "--var cannot be used with --workflow-id; persisted workflow vars are authoritative for existing workflows"
        );
    }
    let vars = super::parse_workflow_vars(&args.vars)?;

    let task_id_for_sync = args.task_id.clone();
    let phase_filter = args.phase.clone();

    let json_for_cb = json;
    let task_id_for_output = args.task_id.clone();
    let requirement_id_for_output = args.requirement_id.clone();
    let on_phase_event: Box<dyn Fn(PhaseEvent<'_>) + Send + Sync> = Box::new(move |event| match event {
        PhaseEvent::Started { phase_id, phase_index, total_phases } => {
            emit_phase_header(phase_id, phase_index, total_phases, json_for_cb);
        }
        PhaseEvent::Decision { decision, .. } => {
            emit_phase_decision(decision, json_for_cb);
        }
        PhaseEvent::Completed { phase_id, duration, success } => {
            emit_phase_footer(phase_id, duration, success, json_for_cb);
        }
    });

    let params = WorkflowExecuteParams {
        project_root: project_root.to_string(),
        workflow_id: args.workflow_id,
        task_id: args.task_id,
        requirement_id: args.requirement_id,
        title: args.title,
        description: args.description,
        workflow_ref: args.workflow_ref,
        input: args.input_json.as_deref().map(serde_json::from_str).transpose()?,
        vars,
        model: args.model,
        tool: args.tool,
        phase_timeout_secs: args.phase_timeout_secs,
        phase_filter: args.phase,
        on_phase_event: Some(on_phase_event),
        hub: Some(hub.clone()),
        phase_routing: None,
        mcp_config: None,
    };

    let result = execute_workflow(params).await?;
    if phase_filter.is_none() {
        if let Some(task_id) = task_id_for_sync.as_deref() {
            project_terminal_workflow_result(
                hub.clone(),
                project_root,
                result.subject_id.as_str(),
                Some(task_id),
                Some(result.workflow_ref.as_str()),
                Some(result.workflow_id.as_str()),
                result.workflow_status,
                None,
            )
            .await;
        }
    }

    emit_workflow_summary(&result.phase_results, result.total_duration, json);

    if json {
        print_value(
            serde_json::json!({
                "workflow_id": result.workflow_id,
                "workflow_ref": result.workflow_ref,
                "workflow_status": result.workflow_status,
                "subject_id": result.subject_id,
                "task_id": task_id_for_output,
                "requirement_id": requirement_id_for_output,
                "execution_cwd": result.execution_cwd,
                "phases_requested": result.phases_requested,
                "total_duration_secs": result.total_duration.as_secs(),
                "results": result.phase_results,
                "post_success": result.post_success,
            }),
            true,
        )
    } else {
        Ok(())
    }
}

fn use_ansi_colors() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

fn emit_phase_header(phase_id: &str, index: usize, total: usize, _json: bool) {
    use std::io::Write as _;
    let color = use_ansi_colors();
    let (bold, cyan, reset) = if color { ("\x1b[1m", "\x1b[36m", "\x1b[0m") } else { ("", "", "") };
    let _ = writeln!(std::io::stderr(), "\n{bold}{cyan}━━━ Phase {}/{}: {} ━━━{reset}", index + 1, total, phase_id,);
}

fn emit_phase_footer(phase_id: &str, duration: Duration, succeeded: bool, _json: bool) {
    use std::io::Write as _;
    let color = use_ansi_colors();
    let dur = format_duration(duration);
    if succeeded {
        let (green, reset) = if color { ("\x1b[32m", "\x1b[0m") } else { ("", "") };
        let _ = writeln!(std::io::stderr(), "{green}completed {phase_id} in {dur}{reset}");
    } else {
        let (red, reset) = if color { ("\x1b[31m", "\x1b[0m") } else { ("", "") };
        let _ = writeln!(std::io::stderr(), "{red}failed {phase_id} in {dur}{reset}");
    }
}

fn emit_phase_decision(decision: &orchestrator_core::PhaseDecision, _json: bool) {
    use std::io::Write as _;
    let color = use_ansi_colors();
    let (dim, cyan, reset) = if color { ("\x1b[2m", "\x1b[36m", "\x1b[0m") } else { ("", "", "") };
    let verdict = match decision.verdict {
        orchestrator_core::PhaseDecisionVerdict::Advance => "advance",
        orchestrator_core::PhaseDecisionVerdict::Rework => "rework",
        orchestrator_core::PhaseDecisionVerdict::Fail => "fail",
        orchestrator_core::PhaseDecisionVerdict::Skip => "skip",
        orchestrator_core::PhaseDecisionVerdict::Unknown => "unknown",
    };
    let confidence_pct = (decision.confidence * 100.0) as u32;
    let _ = writeln!(std::io::stderr(), "{cyan}  verdict: {verdict} ({confidence_pct}% confidence){reset}");
    if !decision.reason.is_empty() {
        let reason = if decision.reason.len() > 120 {
            format!("{}...", &decision.reason[..120])
        } else {
            decision.reason.clone()
        };
        let _ = writeln!(std::io::stderr(), "{dim}  reason: {reason}{reset}");
    }
}

fn emit_workflow_summary(results: &[serde_json::Value], total_duration: Duration, _json: bool) {
    use std::io::Write as _;
    let color = use_ansi_colors();
    let (bold, green, red, dim, reset) =
        if color { ("\x1b[1m", "\x1b[32m", "\x1b[31m", "\x1b[2m", "\x1b[0m") } else { ("", "", "", "", "") };
    let _ = writeln!(std::io::stderr(), "\n{bold}━━━ Workflow Summary ━━━{reset}");
    for r in results {
        let pid = r["phase_id"].as_str().unwrap_or("?");
        let status = r["status"].as_str().unwrap_or("?");
        let dur_secs = r["duration_secs"].as_u64().unwrap_or(0);
        let dur_str = format_duration(Duration::from_secs(dur_secs));
        let (icon, clr) = match status {
            "completed" => ("ok", green),
            "closed" => ("ok", green),
            "rework" => ("↻", dim),
            "manual_pending" => ("hold", dim),
            _ => ("FAIL", red),
        };
        let _ = writeln!(std::io::stderr(), "  {clr}{icon}{reset} {pid}: {dim}{status} ({dur_str}){reset}");
        if status == "failed" {
            if let Some(err) = r["error"].as_str() {
                let err_short = if err.len() > 100 { format!("{}...", &err[..100]) } else { err.to_string() };
                let _ = writeln!(std::io::stderr(), "    {red}{err_short}{reset}");
            }
        }
    }
    let _ = writeln!(std::io::stderr(), "  {bold}Total: {}{reset}", format_duration(total_duration));
}
