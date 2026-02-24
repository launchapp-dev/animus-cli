use super::*;
use anyhow::{anyhow, Result};

use super::model::{GitConfirmationOutcomeCli, GitConfirmationRecordCli};
use super::store::{load_git_confirmations, save_git_confirmations};

pub(super) fn handle_git_confirm(
    command: GitConfirmCommand,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        GitConfirmCommand::Request(args) => {
            let context = args
                .context_json
                .as_deref()
                .map(serde_json::from_str::<Value>)
                .transpose()?
                .unwrap_or_else(|| serde_json::json!({}));
            let operation = args.operation_type.trim().to_ascii_lowercase();
            let required = matches!(
                operation.as_str(),
                "force_push" | "remove_worktree" | "remove_repo" | "hard_reset" | "clean_untracked"
            );
            let blocked = false;
            let reason = if required {
                "destructive operation requires confirmation".to_string()
            } else {
                "operation does not require confirmation".to_string()
            };
            let record = GitConfirmationRecordCli {
                id: format!("confirm-{}", Uuid::new_v4().simple()),
                operation_type: args.operation_type,
                repo_name: args.repo_name,
                context,
                required,
                blocked,
                reason,
                created_at: Utc::now().to_rfc3339(),
                approved: None,
                comment: None,
                user_id: None,
                responded_at: None,
                outcome: None,
            };
            let mut store = load_git_confirmations(project_root)?;
            store.requests.push(record.clone());
            save_git_confirmations(project_root, &store)?;
            print_value(record, json)
        }
        GitConfirmCommand::Respond(args) => {
            let mut store = load_git_confirmations(project_root)?;
            let request = store
                .requests
                .iter_mut()
                .find(|request| request.id == args.request_id)
                .ok_or_else(|| anyhow!("confirmation request not found: {}", args.request_id))?;
            request.approved = Some(args.approved);
            request.comment = args.comment;
            request.user_id = args.user_id;
            request.responded_at = Some(Utc::now().to_rfc3339());
            let approved = request.approved;
            save_git_confirmations(project_root, &store)?;
            print_value(
                serde_json::json!({
                    "request_id": args.request_id,
                    "approved": approved,
                }),
                json,
            )
        }
        GitConfirmCommand::Outcome(args) => {
            let mut store = load_git_confirmations(project_root)?;
            let request = store
                .requests
                .iter_mut()
                .find(|request| request.id == args.request_id)
                .ok_or_else(|| anyhow!("confirmation request not found: {}", args.request_id))?;
            request.outcome = Some(GitConfirmationOutcomeCli {
                success: args.success,
                message: args.message,
                metadata: args
                    .metadata_json
                    .as_deref()
                    .map(serde_json::from_str::<Value>)
                    .transpose()?,
                recorded_at: Utc::now().to_rfc3339(),
            });
            let outcome = request.outcome.clone();
            save_git_confirmations(project_root, &store)?;
            print_value(outcome, json)
        }
    }
}
