#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizedTextEvent {
    TextChunk { text: String },
    FinalResult { text: String },
    Ignored,
}
