//! `animus cost` command tree.
//!
//! The handler scans the live `runs/` directory under the scoped state
//! root, folds metadata events into a fresh `CostState`, and renders
//! the requested view. The cached `cost-state.v1.json` file is updated
//! as a side effect so the daemon's auto-pause hook can read a recent
//! snapshot without re-scanning.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, Utc};
use serde::Serialize;

use crate::cli_types::{
    CostCommand, CostConversationArgs, CostDecisionsArgs, CostSummaryArgs, CostSummaryBy, CostTopArgs, CostTopBy,
    CostTrendWindow, CostTrendsArgs, CostWorkflowArgs, CostWorkflowBy,
};
use crate::services::cost::{
    aggregator::COST_STATE_SCHEMA_ID, enforce_caps, read_decision_records, refresh_cost_state, CostState, PhaseCost,
    WorkflowCost,
};
use crate::services::runtime::runtime_chat::store::{ConversationStore, FileConversationStore};
use crate::{invalid_input_error, not_found_error, print_value};

const SUMMARY_SCHEMA: &str = "animus.cost.summary.v1";
const WORKFLOW_SCHEMA: &str = "animus.cost.workflow.v1";
const TOP_SCHEMA: &str = "animus.cost.top.v1";
const TRENDS_SCHEMA: &str = "animus.cost.trends.v1";
const CONVERSATION_SCHEMA: &str = "animus.cost.conversation.v1";
const SUMMARY_BREAKDOWN_SCHEMA: &str = "animus.cost.summary.breakdown.v1";
const WORKFLOW_BREAKDOWN_SCHEMA: &str = "animus.cost.workflow.breakdown.v1";
const TOP_MODELS_SCHEMA: &str = "animus.cost.top.models.v1";

/// Bucket key used when a phase carries no provider / model attribution
/// (legacy cost-state records, or runs whose checkpoint predates
/// attribution capture).
const UNKNOWN_GROUP: &str = "unknown";

/// One grouped attribution row: total tokens + USD for a single
/// provider or model, plus its share of the grand total.
#[derive(Debug, Clone, Serialize)]
struct GroupedRow {
    /// Provider id, model id, or phase id depending on the grouping.
    key: String,
    total_tokens: u64,
    total_cost_usd: f64,
    /// Share of the grouped grand total cost, 0.0–100.0. Falls back to
    /// the token share when the grand total cost is zero.
    percent: f64,
}

/// Accumulate (tokens, cost) keyed by an attribution dimension, then
/// emit rows sorted by cost (descending) with a percentage of the
/// grand total. A `None` key folds into the [`UNKNOWN_GROUP`] bucket so
/// legacy records still surface.
fn group_rows<'a, I>(entries: I) -> Vec<GroupedRow>
where
    I: IntoIterator<Item = (Option<&'a str>, u64, f64)>,
{
    let mut tally: BTreeMap<String, (u64, f64)> = BTreeMap::new();
    for (key, tokens, cost) in entries {
        let key = key.map(str::trim).filter(|k| !k.is_empty()).unwrap_or(UNKNOWN_GROUP).to_string();
        let slot = tally.entry(key).or_insert((0, 0.0));
        slot.0 = slot.0.saturating_add(tokens);
        slot.1 += cost;
    }
    let grand_cost: f64 = tally.values().map(|(_, cost)| *cost).sum();
    let grand_tokens: u64 = tally.values().map(|(tokens, _)| *tokens).sum();
    let mut rows: Vec<GroupedRow> = tally
        .into_iter()
        .map(|(key, (tokens, cost))| {
            let percent = if grand_cost > 0.0 {
                cost / grand_cost * 100.0
            } else if grand_tokens > 0 {
                tokens as f64 / grand_tokens as f64 * 100.0
            } else {
                0.0
            };
            GroupedRow { key, total_tokens: tokens, total_cost_usd: cost, percent }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.total_cost_usd
            .partial_cmp(&a.total_cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.total_tokens.cmp(&a.total_tokens))
            .then_with(|| a.key.cmp(&b.key))
    });
    rows
}

fn render_grouped_rows(rows: &[GroupedRow], dimension: &str) {
    if rows.is_empty() {
        println!("  (no {dimension} attribution recorded)");
        return;
    }
    for row in rows {
        println!(
            "  {:.<28} ${:>8.4}  {:>10} toks  {:>5.1}%",
            truncate(&row.key, 27),
            row.total_cost_usd,
            row.total_tokens,
            row.percent
        );
    }
}

/// Threshold above which the `unknown` attribution bucket triggers the
/// honesty hint (20% of grouped cost).
const UNKNOWN_ATTRIBUTION_HINT_THRESHOLD: f64 = 20.0;

/// The `unknown` bucket's share of grouped cost (its `percent`), or `None`
/// when there is no unknown bucket. Token-share fallback is already baked
/// into `GroupedRow::percent` by [`group_rows`].
fn unknown_attribution_percent(rows: &[GroupedRow]) -> Option<f64> {
    rows.iter().find(|row| row.key == UNKNOWN_GROUP).map(|row| row.percent)
}

/// Print the attribution-honesty hint when the `unknown` bucket exceeds the
/// threshold: provider plugins that omit attribution leave spend
/// unattributable. `dimension` is the grouping noun (`model` / `provider`);
/// the remediation field name varies with it. Pass the FULL grouped rows
/// (before any `--limit` truncation), so a hidden-but-large `unknown` bucket
/// still trips the hint.
fn print_unknown_attribution_hint(rows: &[GroupedRow], dimension: &str) {
    if let Some(percent) = unknown_attribution_percent(rows) {
        if percent > UNKNOWN_ATTRIBUTION_HINT_THRESHOLD {
            let field = if dimension == "provider" { "provider" } else { "model_id" };
            println!(
                "  note: {percent:.0}% of spend lacks {dimension} attribution; \
                 provider plugins must report {field}"
            );
        }
    }
}
const DECISIONS_SCHEMA: &str = "animus.cost.decisions.v1";

pub(crate) async fn handle_cost(command: CostCommand, project_root: &str, json: bool) -> Result<()> {
    let project_path = Path::new(project_root);
    match command {
        CostCommand::Summary(args) => handle_summary(project_path, args, json),
        CostCommand::Workflow(args) => handle_workflow(project_path, args, json),
        CostCommand::Top(args) => handle_top(project_path, args, json),
        CostCommand::Trends(args) => handle_trends(project_path, args, json),
        CostCommand::Conversation(args) => handle_conversation(project_path, args, json),
        CostCommand::Decisions(args) => handle_decisions(project_path, args, json),
    }
}

#[derive(Debug, Serialize)]
struct ConversationCostView {
    schema: &'static str,
    conversation_id: String,
    assistant_turns: usize,
    total_tokens: u64,
    total_cost_usd: f64,
    input_tokens: u64,
    output_tokens: u64,
}

/// Fold per-turn token + USD spend for a single chat conversation.
///
/// Token totals mirror the workflow aggregator (`input + output + reasoning`)
/// and cost precedence mirrors the run scanner (provider-reported `cost_usd`
/// first, else estimate from the turn's model + tokens via the published
/// rates) so `animus cost conversation` agrees with `animus cost` semantics
/// (codex round-5 P2).
fn aggregate_conversation_cost(
    conversation_id: &str,
    messages: &[crate::services::runtime::runtime_chat::store::ChatMessage],
) -> ConversationCostView {
    let mut assistant_turns = 0usize;
    let mut total_tokens = 0u64;
    let mut total_cost_usd = 0.0f64;
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    for message in messages {
        if message.usage.is_none() && message.cost_usd.is_none() {
            continue;
        }
        assistant_turns += 1;
        let turn_tokens = message.usage.as_ref().map_or(0u64, |usage| {
            u64::from(usage.input) + u64::from(usage.output) + u64::from(usage.reasoning.unwrap_or(0))
        });
        if let Some(usage) = &message.usage {
            input_tokens += u64::from(usage.input);
            output_tokens += u64::from(usage.output);
        }
        total_tokens += turn_tokens;
        let turn_cost = message.cost_usd.or_else(|| {
            message
                .model
                .as_deref()
                .and_then(|model| crate::services::cost::model_rates::estimate_cost_usd(model, turn_tokens))
        });
        if let Some(cost) = turn_cost {
            total_cost_usd += cost;
        }
    }
    ConversationCostView {
        schema: CONVERSATION_SCHEMA,
        conversation_id: conversation_id.to_string(),
        assistant_turns,
        total_tokens,
        total_cost_usd,
        input_tokens,
        output_tokens,
    }
}

/// Aggregate per-turn token + USD spend for a single chat conversation by
/// folding the `usage` / `cost_usd` fields recorded on each assistant
/// [`ChatMessage`](crate::services::runtime::runtime_chat::store::ChatMessage).
fn handle_conversation(project_path: &Path, args: CostConversationArgs, json: bool) -> Result<()> {
    let store = FileConversationStore::for_project(project_path)?;
    if store.load_meta(&args.conversation_id)?.is_none() {
        return Err(not_found_error(format!("conversation '{}' not found", args.conversation_id)));
    }
    let messages = store.load_messages(&args.conversation_id)?;
    let view = aggregate_conversation_cost(&args.conversation_id, &messages);
    if json {
        print_value(&view, json)
    } else {
        println!("animus cost — conversation {}", view.conversation_id);
        println!(
            "  spend:  ${:.4} across {} tokens ({} in / {} out)",
            view.total_cost_usd, view.total_tokens, view.input_tokens, view.output_tokens
        );
        println!("  turns:  {} assistant turns with recorded usage", view.assistant_turns);
        Ok(())
    }
}

fn refresh_state(project_path: &Path) -> Result<CostState> {
    // Merge: scanner produces live workflow rollups, but it cannot
    // see archived workflows (the daemon's auto-pause hook moves
    // completed runs to `history` and clears them from
    // `workflows`). The shared refresh preserves the persisted
    // history so `top` / `trends` see completed runs across daemon
    // restarts, and caches the merged view for downstream readers.
    let state = refresh_cost_state(project_path, |message| eprintln!("warning: {message}"))?;
    // Evaluate declared budget caps against the freshest rollup. Any
    // breach is appended to the scoped `decisions.jsonl` (the fleet
    // view `animus cost decisions` reads). This manual path records
    // only — pausing the breaching workflow, writing the per-run
    // decision record, and notifying are the daemon housekeeping
    // sweep's job (`services::cost::enforcement`). Failure here is
    // non-fatal — surface a warning and continue so the view still
    // renders.
    if let Err(error) = enforce_caps(project_path, &state) {
        eprintln!("warning: failed to evaluate budget caps: {error}");
    }
    Ok(state)
}

#[derive(Debug, Serialize)]
struct DecisionsView {
    schema: &'static str,
    since: Option<String>,
    count: usize,
    records: Vec<crate::services::cost::BudgetExceededRecord>,
}

fn decisions_view(
    mut records: Vec<crate::services::cost::BudgetExceededRecord>,
    since: Option<&str>,
) -> Result<DecisionsView> {
    if let Some(window) = since {
        let cutoff = Utc::now() - parse_duration(window)?;
        records.retain(|record| record.observed_at >= cutoff);
    }
    Ok(DecisionsView { schema: DECISIONS_SCHEMA, since: since.map(str::to_string), count: records.len(), records })
}

/// Read the scoped budget-breach log (`~/.animus/<repo-scope>/decisions.jsonl`)
/// — the fleet-level record stream, distinct from the per-run
/// `runs/<run_id>/decisions.jsonl` that `animus output decisions` renders.
fn handle_decisions(project_path: &Path, args: CostDecisionsArgs, json: bool) -> Result<()> {
    let records = read_decision_records(project_path)?;
    let view = decisions_view(records, args.since.as_deref())?;
    if json {
        return print_value(&view, json);
    }
    match view.since.as_deref() {
        Some(window) => println!("animus cost — budget breaches (last {window})"),
        None => println!("animus cost — budget breaches (all recorded)"),
    }
    if view.records.is_empty() {
        println!("  none recorded");
        return Ok(());
    }
    for record in &view.records {
        let scope = match record.phase_id.as_deref() {
            Some(phase_id) => format!("phase {phase_id}"),
            None => "workflow".to_string(),
        };
        let (actual, budget) = match record.limit_field {
            crate::services::cost::BudgetLimitField::MaxCostUsd => {
                (format!("${:.4}", record.actual), format!("${:.4}", record.budget))
            }
            crate::services::cost::BudgetLimitField::MaxTokens => {
                (format!("{} toks", record.actual as u64), format!("{} toks", record.budget as u64))
            }
        };
        println!(
            "  {}  {}  {} {} exceeded: {} > {}  → {}",
            record.observed_at.format("%Y-%m-%d %H:%M:%S"),
            record.workflow_run_id,
            scope,
            record.limit_field.as_str(),
            actual,
            budget,
            record.on_exceed
        );
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct SummaryView {
    schema: &'static str,
    state_schema: &'static str,
    since: String,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    total_tokens: u64,
    total_cost_usd: f64,
    active_workflows: usize,
    completed_workflows: usize,
    top_workflows: Vec<TopSpenderRow>,
}

#[derive(Debug, Serialize)]
struct TopSpenderRow {
    workflow_run_id: String,
    workflow_id: String,
    total_tokens: u64,
    total_cost_usd: f64,
    status: String,
}

/// Summary semantics: `--since` filters which workflow rollups are
/// included (a workflow is in scope if its `started_at` or
/// `updated_at` falls inside the window), but the per-row token / USD
/// totals are the workflow's lifetime spend, not in-window deltas.
/// Per-event windowing would require keeping a time-series sidecar,
/// which is out of scope for v0.5.5 (TODO(codex-p2)).
fn handle_summary(project_path: &Path, args: CostSummaryArgs, json: bool) -> Result<()> {
    let state = refresh_state(project_path)?;
    let window = args.since.as_deref().unwrap_or("24h");
    let duration = parse_duration(window)?;
    let now = Utc::now();
    let window_start = now - duration;

    if let Some(by) = args.by {
        return handle_summary_breakdown(&state, window, window_start, by, json);
    }

    let mut active_count: usize = 0;
    let mut completed: usize = 0;
    let mut all_rows: Vec<TopSpenderRow> = Vec::new();

    for (run_id, workflow) in &state.workflows {
        let last_seen = workflow.updated_at.unwrap_or(workflow.started_at);
        if last_seen < window_start && workflow.started_at < window_start {
            continue;
        }
        // The scanner can attach a terminal `Completed` or `Failed`
        // status to a workflow whose events log contains a `Finished`
        // / `Error` frame but that the daemon has not yet moved into
        // `history`. Those should NOT count as active — otherwise
        // `summary` reports them in the "active" column and the
        // "completed" column stays at zero.
        match workflow.status {
            crate::services::cost::aggregator::WorkflowCostStatus::Running => active_count += 1,
            _ => completed += 1,
        }
        all_rows.push(TopSpenderRow {
            workflow_run_id: run_id.clone(),
            workflow_id: workflow.workflow_id.clone(),
            total_tokens: workflow.total_tokens,
            total_cost_usd: workflow.total_cost_usd,
            status: format!("{:?}", workflow.status).to_lowercase(),
        });
    }
    for history in &state.history {
        if history.finished_at >= window_start {
            completed += 1;
            all_rows.push(TopSpenderRow {
                workflow_run_id: history.workflow_run_id.clone(),
                workflow_id: history.workflow_id.clone(),
                total_tokens: history.total_tokens,
                total_cost_usd: history.total_cost_usd,
                status: format!("{:?}", history.final_status).to_lowercase(),
            });
        }
    }
    let total_tokens: u64 = all_rows.iter().map(|row| row.total_tokens).sum();
    let total_cost_usd: f64 = all_rows.iter().map(|row| row.total_cost_usd).sum();
    all_rows.sort_by(|a, b| b.total_cost_usd.partial_cmp(&a.total_cost_usd).unwrap_or(std::cmp::Ordering::Equal));
    all_rows.truncate(args.top);
    let view = SummaryView {
        schema: SUMMARY_SCHEMA,
        state_schema: COST_STATE_SCHEMA_ID,
        since: window.to_string(),
        window_start,
        window_end: now,
        total_tokens,
        total_cost_usd,
        active_workflows: active_count,
        completed_workflows: completed,
        top_workflows: all_rows,
    };
    if json {
        print_value(&view, json)
    } else {
        print_summary_text(&view);
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct SummaryBreakdownView {
    schema: &'static str,
    state_schema: &'static str,
    since: String,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    by: &'static str,
    total_tokens: u64,
    total_cost_usd: f64,
    rows: Vec<GroupedRow>,
}

/// Render `cost summary --by provider|model`. Only in-window *active*
/// workflows are attributed: archived `HistorySummary` rows carry no
/// per-phase provider / model detail, so they cannot be split. The
/// non-grouped `summary` view (and `cost top`) still include archived
/// runs; this view documents its narrower scope in the header.
fn handle_summary_breakdown(
    state: &CostState,
    window: &str,
    window_start: DateTime<Utc>,
    by: CostSummaryBy,
    json: bool,
) -> Result<()> {
    let now = Utc::now();
    let mut entries: Vec<(Option<&str>, u64, f64)> = Vec::new();
    for workflow in state.workflows.values() {
        let last_seen = workflow.updated_at.unwrap_or(workflow.started_at);
        if last_seen < window_start && workflow.started_at < window_start {
            continue;
        }
        for phase in workflow.phases.values() {
            let key = match by {
                CostSummaryBy::Provider => phase.provider.as_deref(),
                CostSummaryBy::Model => phase.model.as_deref(),
            };
            entries.push((key, phase.total_tokens(), phase.cost_usd));
        }
    }
    let rows = group_rows(entries);
    let total_tokens: u64 = rows.iter().map(|row| row.total_tokens).sum();
    let total_cost_usd: f64 = rows.iter().map(|row| row.total_cost_usd).sum();
    let by_label: &'static str = match by {
        CostSummaryBy::Provider => "provider",
        CostSummaryBy::Model => "model",
    };
    let view = SummaryBreakdownView {
        schema: SUMMARY_BREAKDOWN_SCHEMA,
        state_schema: COST_STATE_SCHEMA_ID,
        since: window.to_string(),
        window_start,
        window_end: now,
        by: by_label,
        total_tokens,
        total_cost_usd,
        rows,
    };
    if json {
        print_value(&view, json)
    } else {
        println!("animus cost — last {} by {}", view.since, view.by);
        println!(
            "  window: {} → {}",
            view.window_start.format("%Y-%m-%d %H:%M UTC"),
            view.window_end.format("%Y-%m-%d %H:%M UTC")
        );
        println!("  spend:  ${:.4} across {} tokens (active runs only)", view.total_cost_usd, view.total_tokens);
        println!();
        render_grouped_rows(&view.rows, view.by);
        print_unknown_attribution_hint(&view.rows, view.by);
        Ok(())
    }
}

fn print_summary_text(view: &SummaryView) {
    println!("animus cost — last {}", view.since);
    println!(
        "  window:    {} → {}",
        view.window_start.format("%Y-%m-%d %H:%M UTC"),
        view.window_end.format("%Y-%m-%d %H:%M UTC")
    );
    println!(
        "  spend:     ${:.4} across {} tokens (lifetime totals for runs touched in window)",
        view.total_cost_usd, view.total_tokens
    );
    println!("  workflows: {} active, {} completed in window", view.active_workflows, view.completed_workflows);
    if view.top_workflows.is_empty() {
        println!("  no workflow activity in window");
        return;
    }
    println!();
    println!("  top {} by cost:", view.top_workflows.len());
    for row in &view.top_workflows {
        println!(
            "    {:.<48} ${:>8.4}  {:>10} toks  [{}]",
            truncate(&row.workflow_run_id, 47),
            row.total_cost_usd,
            row.total_tokens,
            row.status
        );
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        let taken: String = value.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{taken}…")
    }
}

#[derive(Debug, Serialize)]
struct WorkflowView<'a> {
    schema: &'static str,
    state_schema: &'static str,
    workflow_run_id: &'a str,
    workflow: &'a WorkflowCost,
    phases: Vec<PhaseRow<'a>>,
}

#[derive(Debug, Serialize)]
struct PhaseRow<'a> {
    phase_id: &'a str,
    #[serde(flatten)]
    cost: &'a PhaseCost,
    total_tokens: u64,
}

#[derive(Debug, Serialize)]
struct WorkflowBreakdownView<'a> {
    schema: &'static str,
    state_schema: &'static str,
    workflow_run_id: &'a str,
    workflow_id: &'a str,
    by: &'static str,
    total_tokens: u64,
    total_cost_usd: f64,
    rows: Vec<GroupedRow>,
}

/// Render `cost workflow <id> --by provider|model|phase`: group the
/// workflow's per-phase rollups by the requested attribution dimension.
fn handle_workflow_breakdown(
    workflow_run_id: &str,
    workflow: &WorkflowCost,
    by: CostWorkflowBy,
    json: bool,
) -> Result<()> {
    let entries: Vec<(Option<&str>, u64, f64)> = workflow
        .phases
        .iter()
        .map(|(phase_id, cost)| {
            let key = match by {
                CostWorkflowBy::Provider => cost.provider.as_deref(),
                CostWorkflowBy::Model => cost.model.as_deref(),
                CostWorkflowBy::Phase => Some(phase_id.as_str()),
            };
            (key, cost.total_tokens(), cost.cost_usd)
        })
        .collect();
    let rows = group_rows(entries);
    let by_label: &'static str = match by {
        CostWorkflowBy::Provider => "provider",
        CostWorkflowBy::Model => "model",
        CostWorkflowBy::Phase => "phase",
    };
    let view = WorkflowBreakdownView {
        schema: WORKFLOW_BREAKDOWN_SCHEMA,
        state_schema: COST_STATE_SCHEMA_ID,
        workflow_run_id,
        workflow_id: &workflow.workflow_id,
        by: by_label,
        total_tokens: workflow.total_tokens,
        total_cost_usd: workflow.total_cost_usd,
        rows,
    };
    if json {
        print_value(&view, json)
    } else {
        println!(
            "workflow {} ({}) by {}: ${:.4} / {} tokens",
            view.workflow_run_id, view.workflow_id, view.by, view.total_cost_usd, view.total_tokens
        );
        render_grouped_rows(&view.rows, view.by);
        print_unknown_attribution_hint(&view.rows, view.by);
        Ok(())
    }
}

fn handle_workflow(project_path: &Path, args: CostWorkflowArgs, json: bool) -> Result<()> {
    let state = refresh_state(project_path)?;
    if let Some(workflow) = state.workflows.get(&args.workflow_run_id) {
        if let Some(by) = args.by {
            return handle_workflow_breakdown(&args.workflow_run_id, workflow, by, json);
        }
        let phases = workflow
            .phases
            .iter()
            .map(|(phase_id, cost)| PhaseRow { phase_id, cost, total_tokens: cost.total_tokens() })
            .collect();
        let view = WorkflowView {
            schema: WORKFLOW_SCHEMA,
            state_schema: COST_STATE_SCHEMA_ID,
            workflow_run_id: &args.workflow_run_id,
            workflow,
            phases,
        };
        return if json {
            print_value(&view, json)
        } else {
            println!(
                "workflow {} ({}): ${:.4} / {} tokens",
                view.workflow_run_id,
                view.workflow.workflow_id,
                view.workflow.total_cost_usd,
                view.workflow.total_tokens
            );
            if view.phases.is_empty() {
                println!("  no phase activity yet");
            } else {
                for row in &view.phases {
                    println!(
                        "  {:<24} ${:>8.4}  {:>10} toks  attempt={}{}",
                        row.phase_id,
                        row.cost.cost_usd,
                        row.total_tokens,
                        row.cost.attempts,
                        row.cost.model.as_deref().map(|m| format!(" model={m}")).unwrap_or_default()
                    );
                }
            }
            Ok(())
        };
    }
    // Fall back to the history ring: completed workflows that the
    // daemon's auto-pause hook has archived no longer appear in
    // `state.workflows` but their HistorySummary is still queryable.
    if let Some(history) = state.history.iter().find(|h| h.workflow_run_id == args.workflow_run_id) {
        if args.by.is_some() {
            return Err(invalid_input_error(format!(
                "workflow run '{}' is archived; archived runs do not retain per-phase provider/model detail, \
                 so `--by` is unavailable. Use `cost top --by model` for a cross-run model leaderboard.",
                args.workflow_run_id
            )));
        }
        let view = ArchivedWorkflowView {
            schema: WORKFLOW_SCHEMA,
            state_schema: COST_STATE_SCHEMA_ID,
            workflow_run_id: &args.workflow_run_id,
            workflow_id: &history.workflow_id,
            started_at: history.started_at,
            finished_at: history.finished_at,
            total_tokens: history.total_tokens,
            total_cost_usd: history.total_cost_usd,
            final_status: format!("{:?}", history.final_status).to_lowercase(),
            archived: true,
        };
        return if json {
            print_value(&view, json)
        } else {
            println!(
                "workflow {} ({}): ${:.4} / {} tokens  [archived: {}]",
                view.workflow_run_id, view.workflow_id, view.total_cost_usd, view.total_tokens, view.final_status
            );
            println!("  archived workflows do not retain per-phase detail; use `cost top` for a leaderboard view");
            Ok(())
        };
    }
    Err(not_found_error(format!("workflow run '{}' not found in cost state or history", args.workflow_run_id)))
}

#[derive(Debug, Serialize)]
struct ArchivedWorkflowView<'a> {
    schema: &'static str,
    state_schema: &'static str,
    workflow_run_id: &'a str,
    workflow_id: &'a str,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    total_tokens: u64,
    total_cost_usd: f64,
    final_status: String,
    archived: bool,
}

#[derive(Debug, Serialize)]
struct TopView {
    schema: &'static str,
    state_schema: &'static str,
    by: &'static str,
    rows: Vec<TopSpenderRow>,
}

#[derive(Debug, Serialize)]
struct TopModelsView {
    schema: &'static str,
    state_schema: &'static str,
    by: &'static str,
    limit: usize,
    rows: Vec<GroupedRow>,
}

/// Render `cost top --by model|provider`: a cross-workflow leaderboard of
/// the requested attribution dimension ranked by total USD spend. Only live
/// workflows (which retain per-phase attribution) contribute named rows;
/// archived history runs carry no model/provider detail and fold into the
/// `unknown` bucket.
fn top_grouped_rows(state: &CostState, dimension: &str) -> Vec<GroupedRow> {
    let mut entries: Vec<(Option<&str>, u64, f64)> = Vec::new();
    for workflow in state.workflows.values() {
        for phase in workflow.phases.values() {
            let key = match dimension {
                "provider" => phase.provider.as_deref(),
                _ => phase.model.as_deref(),
            };
            entries.push((key, phase.total_tokens(), phase.cost_usd));
        }
    }
    for history in &state.history {
        entries.push((None, history.total_tokens, history.total_cost_usd));
    }
    // Returns the FULL ranking; the caller truncates to `--limit` for display
    // but keeps the full set so the attribution hint sees a large `unknown`
    // bucket even when it would rank below the cutoff.
    group_rows(entries)
}

fn handle_top_grouped(state: &CostState, dimension: &'static str, limit: usize, json: bool) -> Result<()> {
    let full_rows = top_grouped_rows(state, dimension);
    // Decide the hint from the FULL ranking, before `--limit` can hide a
    // large `unknown` bucket below the cutoff.
    let unknown_percent = unknown_attribution_percent(&full_rows);
    let mut rows = full_rows;
    rows.truncate(limit);
    let view =
        TopModelsView { schema: TOP_MODELS_SCHEMA, state_schema: COST_STATE_SCHEMA_ID, by: dimension, limit, rows };
    if json {
        print_value(&view, json)
    } else {
        println!("animus cost top by {dimension} (limit {})", view.rows.len());
        render_grouped_rows(&view.rows, dimension);
        if unknown_percent.is_some_and(|percent| percent > UNKNOWN_ATTRIBUTION_HINT_THRESHOLD) {
            let field = if dimension == "provider" { "provider" } else { "model_id" };
            println!(
                "  note: {:.0}% of spend lacks {dimension} attribution; provider plugins must report {field}",
                unknown_percent.unwrap()
            );
        }
        Ok(())
    }
}

fn handle_top(project_path: &Path, args: CostTopArgs, json: bool) -> Result<()> {
    let state = refresh_state(project_path)?;
    match args.by {
        CostTopBy::Model => return handle_top_grouped(&state, "model", args.limit, json),
        CostTopBy::Provider => return handle_top_grouped(&state, "provider", args.limit, json),
        CostTopBy::Tokens | CostTopBy::Cost => {}
    }
    let mut rows: Vec<TopSpenderRow> = state
        .workflows
        .iter()
        .map(|(run_id, workflow)| TopSpenderRow {
            workflow_run_id: run_id.clone(),
            workflow_id: workflow.workflow_id.clone(),
            total_tokens: workflow.total_tokens,
            total_cost_usd: workflow.total_cost_usd,
            status: format!("{:?}", workflow.status).to_lowercase(),
        })
        .collect();
    for history in &state.history {
        rows.push(TopSpenderRow {
            workflow_run_id: history.workflow_run_id.clone(),
            workflow_id: history.workflow_id.clone(),
            total_tokens: history.total_tokens,
            total_cost_usd: history.total_cost_usd,
            status: format!("{:?}", history.final_status).to_lowercase(),
        });
    }
    // `CostTopBy::Model` / `Provider` are handled by the early return above.
    let by_label: &'static str = match args.by {
        CostTopBy::Tokens => "tokens",
        CostTopBy::Cost => "cost",
        CostTopBy::Model | CostTopBy::Provider => {
            unreachable!("grouped rankings return early via handle_top_grouped")
        }
    };
    match args.by {
        CostTopBy::Tokens => rows.sort_by_key(|row| std::cmp::Reverse(row.total_tokens)),
        CostTopBy::Cost => {
            rows.sort_by(|a, b| b.total_cost_usd.partial_cmp(&a.total_cost_usd).unwrap_or(std::cmp::Ordering::Equal))
        }
        CostTopBy::Model | CostTopBy::Provider => {
            unreachable!("grouped rankings return early via handle_top_grouped")
        }
    }
    rows.truncate(args.limit);
    let view = TopView { schema: TOP_SCHEMA, state_schema: COST_STATE_SCHEMA_ID, by: by_label, rows };
    if json {
        print_value(&view, json)
    } else {
        println!("animus cost top by {} (limit {})", view.by, view.rows.len());
        if view.rows.is_empty() {
            println!("  (no workflow activity recorded)");
        } else {
            for (idx, row) in view.rows.iter().enumerate() {
                println!(
                    "  {:>2}. {:.<46} ${:>8.4}  {:>10} toks  [{}]",
                    idx + 1,
                    truncate(&row.workflow_run_id, 45),
                    row.total_cost_usd,
                    row.total_tokens,
                    row.status
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct TrendsView {
    schema: &'static str,
    state_schema: &'static str,
    window: &'static str,
    buckets: Vec<TrendBucket>,
}

#[derive(Debug, Serialize)]
struct TrendBucket {
    /// `YYYY-MM-DD` for `day`, `YYYY-Www` for `week`, `YYYY-MM` for `month`.
    label: String,
    total_tokens: u64,
    total_cost_usd: f64,
    workflow_runs: usize,
}

fn handle_trends(project_path: &Path, args: CostTrendsArgs, json: bool) -> Result<()> {
    let state = refresh_state(project_path)?;
    let mut tally: BTreeMap<String, TrendBucket> = BTreeMap::new();
    for workflow in state.workflows.values() {
        let key = bucket_label(workflow.started_at, args.window);
        let entry = tally.entry(key.clone()).or_insert_with(|| TrendBucket {
            label: key,
            total_tokens: 0,
            total_cost_usd: 0.0,
            workflow_runs: 0,
        });
        entry.total_tokens = entry.total_tokens.saturating_add(workflow.total_tokens);
        entry.total_cost_usd += workflow.total_cost_usd;
        entry.workflow_runs += 1;
    }
    for history in &state.history {
        let key = bucket_label(history.finished_at, args.window);
        let entry = tally.entry(key.clone()).or_insert_with(|| TrendBucket {
            label: key,
            total_tokens: 0,
            total_cost_usd: 0.0,
            workflow_runs: 0,
        });
        entry.total_tokens = entry.total_tokens.saturating_add(history.total_tokens);
        entry.total_cost_usd += history.total_cost_usd;
        entry.workflow_runs += 1;
    }
    let mut buckets: Vec<TrendBucket> = tally.into_values().collect();
    buckets.sort_by(|a, b| a.label.cmp(&b.label));
    if buckets.len() > args.n {
        let start = buckets.len() - args.n;
        buckets = buckets.split_off(start);
    }
    let window_label: &'static str = match args.window {
        CostTrendWindow::Day => "day",
        CostTrendWindow::Week => "week",
        CostTrendWindow::Month => "month",
    };
    let view = TrendsView { schema: TRENDS_SCHEMA, state_schema: COST_STATE_SCHEMA_ID, window: window_label, buckets };
    if json {
        print_value(&view, json)
    } else {
        println!("animus cost trends — {} buckets (window={})", view.buckets.len(), view.window);
        for bucket in &view.buckets {
            println!(
                "  {:<10} ${:>8.4}  {:>10} toks  ({} runs)",
                bucket.label, bucket.total_cost_usd, bucket.total_tokens, bucket.workflow_runs
            );
        }
        Ok(())
    }
}

fn bucket_label(ts: DateTime<Utc>, window: CostTrendWindow) -> String {
    match window {
        CostTrendWindow::Day => ts.format("%Y-%m-%d").to_string(),
        CostTrendWindow::Week => {
            let iso = ts.iso_week();
            format!("{}-W{:02}", iso.year(), iso.week())
        }
        CostTrendWindow::Month => ts.format("%Y-%m").to_string(),
    }
}

fn parse_duration(input: &str) -> Result<Duration> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(invalid_input_error("--since duration must not be empty"));
    }
    let (num_part, unit_part) =
        trimmed.split_at(trimmed.find(|c: char| c.is_ascii_alphabetic()).ok_or_else(|| {
            invalid_input_error(format!("--since duration '{trimmed}' must end with a unit (m/h/d/w)"))
        })?);
    let value: i64 = num_part
        .parse()
        .map_err(|_| invalid_input_error(format!("--since duration '{trimmed}' has invalid number")))?;
    if value <= 0 {
        return Err(invalid_input_error("--since duration must be positive"));
    }
    match unit_part {
        "m" => Ok(Duration::minutes(value)),
        "h" => Ok(Duration::hours(value)),
        "d" => Ok(Duration::days(value)),
        "w" => Ok(Duration::weeks(value)),
        other => Err(invalid_input_error(format!("--since duration unit '{other}' must be one of m/h/d/w"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::runtime::runtime_chat::store::{ChatMessage, ChatRole};

    fn assistant_turn(input: u32, output: u32, reasoning: Option<u32>, model: &str, cost: Option<f64>) -> ChatMessage {
        ChatMessage {
            seq: 1,
            role: ChatRole::Assistant,
            content: "reply".into(),
            recorded_at: "2026-06-08T00:00:00Z".into(),
            tool: Some("claude".into()),
            model: Some(model.into()),
            usage: Some(protocol::TokenUsage { input, output, reasoning, cache_read: None, cache_write: None }),
            cost_usd: cost,
            blocks: Vec::new(),
        }
    }

    #[test]
    fn conversation_cost_includes_reasoning_tokens() {
        // input 100 + output 40 + reasoning 60 = 200 total tokens.
        let messages = vec![assistant_turn(100, 40, Some(60), "claude-sonnet-4-6", Some(0.01))];
        let view = aggregate_conversation_cost("c1", &messages);
        assert_eq!(view.total_tokens, 200, "reasoning tokens must be included in the total");
        assert_eq!(view.input_tokens, 100);
        assert_eq!(view.output_tokens, 40);
        assert_eq!(view.assistant_turns, 1);
    }

    #[test]
    fn conversation_cost_estimates_from_model_when_provider_omits_cost() {
        // No cost_usd, but model + tokens are known: 1M sonnet tokens @ $6/M.
        let messages = vec![assistant_turn(600_000, 400_000, None, "claude-sonnet-4-6", None)];
        let view = aggregate_conversation_cost("c1", &messages);
        assert_eq!(view.total_tokens, 1_000_000);
        assert!((view.total_cost_usd - 6.0).abs() < 1e-6, "expected $6 estimate, got {}", view.total_cost_usd);
    }

    #[test]
    fn conversation_cost_prefers_provider_reported_cost() {
        let messages = vec![assistant_turn(1000, 1000, None, "claude-sonnet-4-6", Some(0.25))];
        let view = aggregate_conversation_cost("c1", &messages);
        assert!((view.total_cost_usd - 0.25).abs() < 1e-9, "provider cost must win over estimate");
    }

    #[test]
    fn conversation_cost_skips_user_only_turns() {
        let user = ChatMessage {
            seq: 0,
            role: ChatRole::User,
            content: "hi".into(),
            recorded_at: "2026-06-08T00:00:00Z".into(),
            tool: None,
            model: None,
            usage: None,
            cost_usd: None,
            blocks: Vec::new(),
        };
        let view = aggregate_conversation_cost("c1", &[user]);
        assert_eq!(view.assistant_turns, 0);
        assert_eq!(view.total_tokens, 0);
        assert_eq!(view.total_cost_usd, 0.0);
    }

    #[test]
    fn parse_duration_accepts_supported_units() {
        assert_eq!(parse_duration("30m").unwrap(), Duration::minutes(30));
        assert_eq!(parse_duration("12h").unwrap(), Duration::hours(12));
        assert_eq!(parse_duration("7d").unwrap(), Duration::days(7));
        assert_eq!(parse_duration("2w").unwrap(), Duration::weeks(2));
    }

    #[test]
    fn parse_duration_rejects_non_positive() {
        assert!(parse_duration("0h").is_err());
        assert!(parse_duration("-5m").is_err());
    }

    #[test]
    fn parse_duration_rejects_unknown_unit() {
        assert!(parse_duration("5y").is_err());
        assert!(parse_duration("5").is_err());
    }

    #[test]
    fn group_rows_sums_and_computes_cost_percentages() {
        // claude: 200 tok / $0.75 ; gemini: 100 tok / $0.25 → 75% / 25%.
        let rows = group_rows([(Some("claude"), 100, 0.50), (Some("claude"), 100, 0.25), (Some("gemini"), 100, 0.25)]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].key, "claude");
        assert_eq!(rows[0].total_tokens, 200);
        assert!((rows[0].total_cost_usd - 0.75).abs() < 1e-9);
        assert!((rows[0].percent - 75.0).abs() < 1e-6);
        assert_eq!(rows[1].key, "gemini");
        assert!((rows[1].percent - 25.0).abs() < 1e-6);
    }

    #[test]
    fn group_rows_buckets_none_and_empty_under_unknown() {
        let rows = group_rows([(None, 50, 0.10), (Some(""), 50, 0.10), (Some("  "), 0, 0.0)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, UNKNOWN_GROUP);
        assert_eq!(rows[0].total_tokens, 100);
        assert!((rows[0].total_cost_usd - 0.20).abs() < 1e-9);
        assert!((rows[0].percent - 100.0).abs() < 1e-6);
    }

    #[test]
    fn group_rows_falls_back_to_token_share_when_cost_is_zero() {
        // No USD reported anywhere → percentage uses token share.
        let rows = group_rows([(Some("claude"), 300, 0.0), (Some("codex"), 100, 0.0)]);
        assert_eq!(rows[0].key, "claude");
        assert!((rows[0].percent - 75.0).abs() < 1e-6);
        assert!((rows[1].percent - 25.0).abs() < 1e-6);
    }

    #[test]
    fn old_phase_cost_without_provider_deserializes() {
        // A cost-state PhaseCost written before attribution capture has
        // no `provider` field; it must load with `provider == None`.
        let json = r#"{"tokens_input":10,"tokens_output":20,"cost_usd":0.01,"model":"claude-sonnet-4-6"}"#;
        let phase: crate::services::cost::PhaseCost = serde_json::from_str(json).unwrap();
        assert_eq!(phase.tokens_input, 10);
        assert_eq!(phase.model.as_deref(), Some("claude-sonnet-4-6"));
        assert!(phase.provider.is_none());
        assert_eq!(phase.attempts, 1, "default attempts back-fills to 1");
    }

    #[test]
    fn decisions_view_filters_by_since_window() {
        use crate::services::cost::{
            BudgetExceededRecord, BudgetLimitField, BudgetLimitKind, BUDGET_EXCEEDED_SCHEMA_ID,
        };
        let record = |observed_at: chrono::DateTime<Utc>| BudgetExceededRecord {
            schema: BUDGET_EXCEEDED_SCHEMA_ID.to_string(),
            workflow_run_id: "wf-run".to_string(),
            workflow_id: "wf-run".to_string(),
            phase_id: None,
            limit_kind: BudgetLimitKind::Workflow,
            limit_field: BudgetLimitField::MaxCostUsd,
            actual: 6.0,
            budget: 5.0,
            on_exceed: "pause".to_string(),
            observed_at,
        };
        let now = Utc::now();
        let records = vec![record(now - Duration::days(3)), record(now - Duration::minutes(5))];

        let unfiltered = decisions_view(records.clone(), None).unwrap();
        assert_eq!(unfiltered.count, 2);

        let filtered = decisions_view(records, Some("24h")).unwrap();
        assert_eq!(filtered.count, 1, "only the recent breach falls inside the 24h window");
        assert_eq!(filtered.since.as_deref(), Some("24h"));

        assert!(decisions_view(Vec::new(), Some("bogus")).is_err(), "invalid duration must error");
    }

    #[test]
    fn top_grouped_rows_ranks_providers_across_workflows() {
        use crate::services::cost::aggregator::{MetadataDelta, WorkflowCost};
        let now = Utc::now();
        let mut state = CostState::default();
        let mut wf_a = WorkflowCost::new("flow-a", now);
        wf_a.record_metadata(
            "impl",
            now,
            MetadataDelta {
                cost_usd: 0.30,
                provider: Some("claude".into()),
                model: Some("claude-x".into()),
                ..Default::default()
            },
        );
        let mut wf_b = WorkflowCost::new("flow-b", now);
        wf_b.record_metadata(
            "impl",
            now,
            MetadataDelta {
                cost_usd: 0.70,
                provider: Some("codex".into()),
                model: Some("gpt-x".into()),
                ..Default::default()
            },
        );
        wf_b.record_metadata(
            "review",
            now,
            MetadataDelta {
                cost_usd: 0.10,
                provider: Some("claude".into()),
                model: Some("claude-y".into()),
                ..Default::default()
            },
        );
        state.workflows.insert("wf-a".into(), wf_a);
        state.workflows.insert("wf-b".into(), wf_b);

        let rows = top_grouped_rows(&state, "provider");
        assert_eq!(rows.len(), 2, "two providers across the two workflows");
        assert_eq!(rows[0].key, "codex", "codex leads on $0.70 cost");
        assert!((rows[0].total_cost_usd - 0.70).abs() < 1e-9);
        assert_eq!(rows[1].key, "claude");
        assert!((rows[1].total_cost_usd - 0.40).abs() < 1e-9, "claude folds 0.30 + 0.10 across workflows");
    }

    #[test]
    fn top_grouped_rows_folds_missing_provider_into_unknown() {
        use crate::services::cost::aggregator::{MetadataDelta, WorkflowCost};
        let now = Utc::now();
        let mut state = CostState::default();
        let mut wf = WorkflowCost::new("flow", now);
        wf.record_metadata("impl", now, MetadataDelta { cost_usd: 0.50, provider: None, ..Default::default() });
        state.workflows.insert("wf".into(), wf);
        let rows = top_grouped_rows(&state, "provider");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, UNKNOWN_GROUP);
    }

    #[test]
    fn unknown_attribution_hint_fires_above_threshold() {
        // 30% unknown → above the 20% threshold.
        let rows = group_rows([(Some("claude"), 700, 0.70), (None, 300, 0.30)]);
        let percent = unknown_attribution_percent(&rows).unwrap();
        assert!((percent - 30.0).abs() < 1e-6);
        assert!(percent > UNKNOWN_ATTRIBUTION_HINT_THRESHOLD, "30% must trip the hint");
    }

    #[test]
    fn unknown_attribution_hint_silent_at_or_below_threshold() {
        // Exactly 20% unknown → NOT strictly greater than the threshold.
        let rows = group_rows([(Some("claude"), 800, 0.80), (None, 200, 0.20)]);
        let percent = unknown_attribution_percent(&rows).unwrap();
        assert!((percent - 20.0).abs() < 1e-6);
        assert!(percent <= UNKNOWN_ATTRIBUTION_HINT_THRESHOLD, "20% must not trip the hint");
    }

    #[test]
    fn unknown_attribution_percent_absent_when_fully_attributed() {
        let rows = group_rows([(Some("claude"), 700, 0.70), (Some("gemini"), 300, 0.30)]);
        assert!(unknown_attribution_percent(&rows).is_none(), "no unknown bucket → no hint");
    }

    #[test]
    fn bucket_label_matches_window() {
        let ts = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 6, 5, 12, 0, 0).unwrap();
        assert_eq!(bucket_label(ts, CostTrendWindow::Day), "2026-06-05");
        assert_eq!(bucket_label(ts, CostTrendWindow::Month), "2026-06");
        assert!(bucket_label(ts, CostTrendWindow::Week).starts_with("2026-W"));
    }
}
