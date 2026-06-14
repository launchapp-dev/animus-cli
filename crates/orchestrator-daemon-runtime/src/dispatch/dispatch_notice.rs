use protocol::SubjectDispatch;

use crate::DispatchSelectionSource;

#[derive(Debug, Clone, PartialEq)]
pub enum DispatchNotice {
    Started {
        dispatch: SubjectDispatch,
        selection_source: DispatchSelectionSource,
    },
    Failed {
        dispatch: SubjectDispatch,
        error: String,
    },
    /// Spawn was rejected recoverably (workflow concurrency cap). The entry
    /// must stay queued / be released back to pending for the next tick —
    /// it is NOT a terminal failure.
    Deferred {
        dispatch: SubjectDispatch,
        reason: String,
    },
    ScheduleDispatched {
        schedule_id: String,
        dispatch: SubjectDispatch,
    },
    ScheduleDispatchFailed {
        schedule_id: String,
        dispatch: SubjectDispatch,
        error: String,
    },
}

pub trait DispatchNoticeSink {
    fn notice(&mut self, notice: DispatchNotice);
}
