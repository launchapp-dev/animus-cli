use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct CliSuccessEnvelope<T: Serialize> {
    pub schema: &'static str,
    pub ok: bool,
    pub data: T,
}
