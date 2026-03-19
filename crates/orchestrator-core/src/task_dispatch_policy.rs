use chrono::Utc;
use protocol::ModelRoutingComplexity;

use crate::{Complexity, OrchestratorTask, STANDARD_WORKFLOW_REF, UI_UX_WORKFLOW_REF};

pub fn routing_complexity_for_task(task: &OrchestratorTask) -> Option<ModelRoutingComplexity> {
    match task.complexity {
        Complexity::Low => Some(ModelRoutingComplexity::Low),
        Complexity::Medium => Some(ModelRoutingComplexity::Medium),
        Complexity::High => Some(ModelRoutingComplexity::High),
    }
}

pub fn workflow_ref_for_task(task: &OrchestratorTask) -> String {
    if task.is_frontend_related() {
        UI_UX_WORKFLOW_REF.to_string()
    } else {
        STANDARD_WORKFLOW_REF.to_string()
    }
}

pub fn should_skip_task_dispatch(task: &OrchestratorTask) -> bool {
    const MAX_DISPATCH_RETRIES: u32 = 3;
    const MIN_RETRY_DELAY_SECS: i64 = 60;

    if let Some(rate_limited_until) = task.rate_limited_until {
        if Utc::now() < rate_limited_until {
            return true;
        }
    }
    if let Some(count) = task.consecutive_dispatch_failures {
        if count >= MAX_DISPATCH_RETRIES {
            return true;
        }
    }
    if let Some(ref last_failure) = task.last_dispatch_failure_at {
        if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(last_failure) {
            let elapsed = Utc::now().signed_duration_since(parsed.with_timezone(&Utc));
            if elapsed.num_seconds() < MIN_RETRY_DELAY_SECS {
                return true;
            }
        }
    }
    false
}
