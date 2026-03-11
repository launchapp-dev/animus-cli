#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionBackendKind {
    ClaudeSdk,
    CodexSdk,
    GeminiSdk,
    Subprocess,
}
