use tokio::sync::mpsc;

use super::session_event::SessionEvent;

#[derive(Debug)]
pub struct SessionRun {
    pub session_id: Option<String>,
    pub events: mpsc::Receiver<SessionEvent>,
    pub selected_backend: String,
    pub fallback_reason: Option<String>,
    pub child_pid: Option<u32>,
}
