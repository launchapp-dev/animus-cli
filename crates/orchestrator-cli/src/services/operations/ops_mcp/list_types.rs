use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ListSizeGuardMode {
    Full,
    SummaryFields,
    SummaryOnly,
}

impl ListSizeGuardMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::SummaryFields => "summary_fields",
            Self::SummaryOnly => "summary_only",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ListToolProfile {
    pub(super) summary_fields: &'static [&'static str],
    pub(super) digest_id_fields: &'static [&'static str],
    pub(super) digest_status_fields: &'static [&'static str],
}

#[derive(Debug, Clone)]
pub(super) struct ListSizeGuardResult {
    pub(super) items: Vec<Value>,
    pub(super) estimated_tokens: usize,
    pub(super) mode: ListSizeGuardMode,
    pub(super) truncated: bool,
}
