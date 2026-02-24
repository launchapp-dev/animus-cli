use anyhow::{bail, Result};
use protocol::Config;
use tokio_tungstenite::tungstenite::handshake::server::Request;

pub fn validate_bearer(req: &Request) -> Result<()> {
    let header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();

    let expected = Config::load_global()
        .map(|c| c.get_token())
        .unwrap_or_else(|_| "dev-token".to_string());
    let token = header.strip_prefix("Bearer ").unwrap_or("");
    if token.is_empty() || token != expected {
        bail!("Unauthorized");
    }
    Ok(())
}
