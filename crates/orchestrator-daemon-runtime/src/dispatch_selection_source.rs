use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchSelectionSource {
    DispatchQueue,
    FallbackPicker,
}

impl DispatchSelectionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DispatchQueue => "dispatch_queue",
            Self::FallbackPicker => "fallback_picker",
        }
    }
}
