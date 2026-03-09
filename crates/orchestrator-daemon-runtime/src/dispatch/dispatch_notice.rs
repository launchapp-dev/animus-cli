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
    QueueAssignmentFailed {
        dispatch: SubjectDispatch,
        error: String,
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

#[derive(Default)]
pub struct NoopDispatchNoticeSink;

impl DispatchNoticeSink for NoopDispatchNoticeSink {
    fn notice(&mut self, _notice: DispatchNotice) {}
}
