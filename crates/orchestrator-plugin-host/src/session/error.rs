use animus_session_backend::error::Error as UpstreamError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("plugin '{plugin}' does not support capability '{capability}'")]
    CapabilityNotSupported { plugin: String, capability: String },
    #[error(transparent)]
    Upstream(#[from] UpstreamError),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn execution_failed(msg: impl Into<String>) -> Self {
        Self::Upstream(UpstreamError::ExecutionFailed(msg.into()))
    }
}

// Collapses local-only variants onto upstream for trait-boundary compatibility.
impl From<Error> for UpstreamError {
    fn from(e: Error) -> Self {
        match e {
            Error::CapabilityNotSupported { plugin, capability } => {
                UpstreamError::ExecutionFailed(format!("plugin '{plugin}' does not support capability '{capability}'"))
            }
            Error::Upstream(u) => u,
        }
    }
}
