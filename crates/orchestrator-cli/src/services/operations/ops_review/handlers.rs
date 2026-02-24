use anyhow::Result;
use chrono::Utc;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

use orchestrator_core::services::ServiceHub;

use crate::{print_value, ReviewCommand};

use super::state::{
    compute_entity_review_status, load_reviews, parse_review_decision, parse_review_entity_type,
    parse_reviewer_role, save_reviews, ReviewDecisionCli, ReviewEntityTypeCli, ReviewRecordCli,
    ReviewerRoleCli,
};

pub(crate) async fn handle_review(
    command: ReviewCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        ReviewCommand::Entity(args) => {
            let store = load_reviews(project_root)?;
            let entity_type = parse_review_entity_type(&args.entity_type)?;
            let decisions: Vec<_> = store
                .reviews
                .into_iter()
                .filter(|review| {
                    review.entity_type == entity_type && review.entity_id == args.entity_id
                })
                .collect();
            print_value(decisions, json)
        }
        ReviewCommand::Record(args) => {
            let mut store = load_reviews(project_root)?;
            let record = ReviewRecordCli {
                id: format!("REV-{}", Uuid::new_v4().simple()),
                entity_type: parse_review_entity_type(&args.entity_type)?,
                entity_id: args.entity_id,
                reviewer_role: parse_reviewer_role(&args.reviewer_role)?,
                decision: parse_review_decision(&args.decision)?,
                source: args.source.unwrap_or_else(|| "manual".to_string()),
                rationale: args
                    .rationale
                    .unwrap_or_else(|| "no rationale provided".to_string()),
                content_hash: args.content_hash,
                created_at: Utc::now().to_rfc3339(),
            };
            store.reviews.push(record.clone());
            save_reviews(project_root, &store)?;
            print_value(record, json)
        }
        ReviewCommand::TaskStatus(args) => {
            let store = load_reviews(project_root)?;
            let status =
                compute_entity_review_status(&store, ReviewEntityTypeCli::Task, &args.task_id);
            print_value(status, json)
        }
        ReviewCommand::RequirementStatus(args) => {
            let store = load_reviews(project_root)?;
            let status =
                compute_entity_review_status(&store, ReviewEntityTypeCli::Requirement, &args.id);
            print_value(status, json)
        }
        ReviewCommand::Handoff(args) => {
            let target_role =
                orchestrator_core::HandoffTargetRole::try_from(args.target_role.as_str())
                    .map_err(|error| anyhow::anyhow!("invalid target role: {}", error))?;
            let handoff = hub
                .review()
                .request_handoff(orchestrator_core::AgentHandoffRequestInput {
                    handoff_id: None,
                    run_id: args.run_id,
                    target_role,
                    question: args.question,
                    context: args
                        .context_json
                        .as_deref()
                        .map(serde_json::from_str::<Value>)
                        .transpose()?
                        .unwrap_or_else(|| serde_json::json!({})),
                })
                .await?;
            print_value(handoff, json)
        }
        ReviewCommand::DualApprove(args) => {
            let mut store = load_reviews(project_root)?;
            let rationale = args
                .rationale
                .unwrap_or_else(|| "Manual dual signoff requested".to_string());
            let entity_id = args.task_id;
            let mut push_if_missing = |role: ReviewerRoleCli, label: &str| {
                let already_approved = store.reviews.iter().rev().find(|review| {
                    review.entity_type == ReviewEntityTypeCli::Task
                        && review.entity_id == entity_id
                        && review.reviewer_role == role
                });
                if already_approved
                    .map(|review| review.decision == ReviewDecisionCli::Approve)
                    .unwrap_or(false)
                {
                    return;
                }
                store.reviews.push(ReviewRecordCli {
                    id: format!("REV-{}", Uuid::new_v4().simple()),
                    entity_type: ReviewEntityTypeCli::Task,
                    entity_id: entity_id.clone(),
                    reviewer_role: role,
                    decision: ReviewDecisionCli::Approve,
                    source: "manual".to_string(),
                    rationale: format!("{rationale} ({label})"),
                    content_hash: None,
                    created_at: Utc::now().to_rfc3339(),
                });
            };
            push_if_missing(ReviewerRoleCli::Po, "PO");
            push_if_missing(ReviewerRoleCli::Em, "EM");
            save_reviews(project_root, &store)?;
            let status =
                compute_entity_review_status(&store, ReviewEntityTypeCli::Task, &entity_id);
            print_value(status, json)
        }
    }
}
