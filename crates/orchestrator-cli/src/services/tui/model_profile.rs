#[derive(Debug, Clone)]
pub(crate) struct ModelProfile {
    pub(crate) model_id: String,
    pub(crate) tool: String,
    pub(crate) availability: String,
    pub(crate) details: Option<String>,
}

impl ModelProfile {
    pub(crate) fn is_available(&self) -> bool {
        self.availability == "available"
    }

    pub(crate) fn label(&self) -> String {
        format!("{} [{}] {}", self.tool, self.model_id, self.availability)
    }
}
