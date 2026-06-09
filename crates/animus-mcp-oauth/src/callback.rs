//! Loopback redirect-URI listener for the interactive OAuth flow.
//!
//! Binds an ephemeral `127.0.0.1:<port>` socket (loopback only — the
//! authorization code never leaves the machine), waits for the single
//! browser redirect, parses `code` + `state` from the query string, and
//! hands them back. The `state` (CSRF token) is matched against the value
//! issued at authorization-URL generation by the caller; a mismatch is a
//! hard error. The listener is bounded by a timeout so an abandoned login
//! cannot hang the CLI forever.
//!
//! Neither the authorization code nor the state is ever logged.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::CALLBACK_HOST;

/// A bound loopback callback listener plus its redirect URI.
pub struct CallbackListener {
    listener: TcpListener,
    redirect_uri: String,
}

/// The captured authorization result from the browser redirect.
#[derive(Debug)]
pub struct CallbackResult {
    pub code: String,
    pub state: String,
}

impl CallbackListener {
    /// Bind an ephemeral loopback port for the redirect URI.
    pub async fn bind() -> Result<Self> {
        let listener =
            TcpListener::bind((CALLBACK_HOST, 0)).await.context("failed to bind loopback OAuth callback listener")?;
        let port = listener.local_addr().context("callback listener has no local addr")?.port();
        let redirect_uri = format!("http://{CALLBACK_HOST}:{port}/callback");
        Ok(Self { listener, redirect_uri })
    }

    /// The `redirect_uri` to register with the authorization server.
    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Wait for the browser redirect (bounded by `timeout`) and return the
    /// captured `code` + `state`. `expected_state` is the CSRF token issued
    /// when the authorization URL was generated; a mismatch is rejected
    /// before the code is returned.
    pub async fn wait_for_code(self, expected_state: &str, timeout: Duration) -> Result<CallbackResult> {
        let accept = async {
            loop {
                let (stream, addr) = self.listener.accept().await.context("callback accept failed")?;
                // Defense in depth: only honor loopback peers even though
                // the socket is bound to 127.0.0.1.
                if !addr.ip().is_loopback() {
                    continue;
                }
                match handle_connection(stream, expected_state).await {
                    Ok(Some(result)) => return Ok(result),
                    // Favicon / probe requests with no code: keep waiting.
                    Ok(None) => continue,
                    Err(err) => return Err(err),
                }
            }
        };

        match tokio::time::timeout(timeout, accept).await {
            Ok(result) => result,
            Err(_) => bail!(
                "timed out after {}s waiting for the OAuth browser redirect; re-run `animus mcp auth` to retry",
                timeout.as_secs()
            ),
        }
    }
}

async fn handle_connection(mut stream: TcpStream, expected_state: &str) -> Result<Option<CallbackResult>> {
    // Read just the request line + headers (we only need the GET path).
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf).await.context("failed to read callback request")?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let request_line = request.lines().next().unwrap_or_default();

    let path = request_line.split_whitespace().nth(1).unwrap_or_default();
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or_default();

    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut oauth_error: Option<String> = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else { continue };
        let decoded = percent_decode(value);
        match key {
            "code" => code = Some(decoded),
            "state" => state = Some(decoded),
            "error" => oauth_error = Some(decoded),
            _ => {}
        }
    }

    if let Some(err) = oauth_error {
        // The authorization server denied the request. Tell the browser,
        // then bubble up. (The error code is not a secret.)
        write_response(&mut stream, "Authorization failed. You can close this tab.").await;
        bail!("authorization server returned error: {err}");
    }

    let (Some(code), Some(state)) = (code, state) else {
        // Not the redirect we're waiting for (e.g. a favicon probe).
        write_response(&mut stream, "Waiting for authorization...").await;
        return Ok(None);
    };

    if state != expected_state {
        write_response(&mut stream, "State mismatch. You can close this tab.").await;
        return Err(anyhow!("OAuth state mismatch: redirect state did not match the issued CSRF token"));
    }

    write_response(&mut stream, "Authorization complete. You can close this tab and return to the terminal.").await;
    Ok(Some(CallbackResult { code, state }))
}

async fn write_response(stream: &mut TcpStream, body: &str) {
    let html = format!("<!doctype html><html><body><p>{body}</p></body></html>");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
}

/// Minimal `application/x-www-form-urlencoded` percent-decoder for query
/// values. Handles `%XX` escapes and `+` → space.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn percent_decode_handles_escapes_and_plus() {
        assert_eq!(percent_decode("a%2Bb"), "a+b");
        assert_eq!(percent_decode("hello+world"), "hello world");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("scope%3Arepo"), "scope:repo");
    }

    #[tokio::test]
    async fn binds_loopback_only() {
        let listener = CallbackListener::bind().await.unwrap();
        assert!(listener.redirect_uri().starts_with("http://127.0.0.1:"));
        let addr = listener.listener.local_addr().unwrap();
        assert!(addr.ip().is_loopback(), "callback must bind loopback only");
    }

    #[tokio::test]
    async fn rejects_state_mismatch() {
        let listener = CallbackListener::bind().await.unwrap();
        let redirect = listener.redirect_uri().to_string();

        let server = tokio::spawn(async move { listener.wait_for_code("expected-csrf", Duration::from_secs(5)).await });

        // Drive a redirect with the WRONG state.
        let url = format!("{redirect}?code=the-code&state=wrong-csrf");
        let _ = send_get(&url).await;

        let result = server.await.unwrap();
        assert!(result.is_err(), "state mismatch must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("state mismatch"), "unexpected error: {msg}");
    }

    #[tokio::test]
    async fn accepts_matching_state_and_returns_code() {
        let listener = CallbackListener::bind().await.unwrap();
        let redirect = listener.redirect_uri().to_string();

        let server = tokio::spawn(async move { listener.wait_for_code("good-csrf", Duration::from_secs(5)).await });

        let url = format!("{redirect}?code=auth-code-123&state=good-csrf");
        let _ = send_get(&url).await;

        let result = server.await.unwrap().expect("should capture code");
        assert_eq!(result.code, "auth-code-123");
        assert_eq!(result.state, "good-csrf");
    }

    /// Issue a bare HTTP/1.0 GET to `url` over a raw TCP socket (no reqwest
    /// dependency for this loopback test).
    async fn send_get(url: &str) -> std::io::Result<()> {
        let parsed = url::Url::parse(url).unwrap();
        let host = parsed.host_str().unwrap();
        let port = parsed.port().unwrap();
        let path_and_query = match parsed.query() {
            Some(q) => format!("{}?{}", parsed.path(), q),
            None => parsed.path().to_string(),
        };
        let mut stream = TcpStream::connect((host, port)).await?;
        let req = format!("GET {path_and_query} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).await?;
        stream.flush().await?;
        let mut sink = Vec::new();
        let _ = stream.read_to_end(&mut sink).await;
        Ok(())
    }
}
