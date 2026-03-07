use chrono::{DateTime, Local, NaiveTime, Utc};

#[derive(Debug, Clone)]
pub struct ProjectTickTime {
    schedule_at: DateTime<Utc>,
    local_time: NaiveTime,
}

impl ProjectTickTime {
    pub fn now() -> Self {
        Self::from_utc(Utc::now())
    }

    pub fn from_utc(schedule_at: DateTime<Utc>) -> Self {
        let local_time = schedule_at.with_timezone(&Local).time();
        Self {
            schedule_at,
            local_time,
        }
    }

    pub fn schedule_at(&self) -> DateTime<Utc> {
        self.schedule_at
    }

    pub fn local_time(&self) -> NaiveTime {
        self.local_time
    }
}
