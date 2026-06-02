use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Stdio transport for JSON-RPC framing.
///
/// Issue #241: the reader uses a streaming serde deserializer so it
/// accepts both NDJSON and pretty-printed multi-line frames.
pub struct StdioTransport<R, W> {
    reader: R,
    writer: W,
    buffer: Vec<u8>,
}

impl<R, W> StdioTransport<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self { reader, writer, buffer: Vec::with_capacity(8 * 1024) }
    }

    pub async fn read_message<T>(&mut self) -> Result<Option<T>>
    where
        T: for<'de> Deserialize<'de>,
    {
        let mut chunk = [0u8; 4096];
        loop {
            // Try to peel a frame from whatever is already buffered.
            let leading_ws = self.buffer.iter().take_while(|b| b.is_ascii_whitespace()).count();
            if leading_ws > 0 {
                self.buffer.drain(..leading_ws);
            }
            if !self.buffer.is_empty() {
                let mut stream = serde_json::Deserializer::from_slice(&self.buffer).into_iter::<T>();
                match stream.next() {
                    Some(Ok(value)) => {
                        let consumed = stream.byte_offset();
                        drop(stream);
                        self.buffer.drain(..consumed);
                        return Ok(Some(value));
                    }
                    Some(Err(error)) if error.is_eof() => {
                        // fall through to read more bytes
                        drop(stream);
                    }
                    Some(Err(error)) => {
                        drop(stream);
                        // Surface the parse error to the caller. The
                        // public StdioTransport contract has always
                        // returned `Err(_)` for malformed frames; the
                        // streaming reader preserves that.  We discard
                        // any buffered bytes up to the next newline so
                        // a subsequent call can recover from the bad
                        // frame instead of failing in a loop.
                        if let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
                            self.buffer.drain(..=pos);
                        } else {
                            self.buffer.clear();
                        }
                        return Err(error.into());
                    }
                    None => {
                        drop(stream);
                    }
                }
            }

            let n = self.reader.read(&mut chunk).await?;
            if n == 0 {
                // Peer closed.  If we had partial bytes buffered for a
                // frame, surface a truncation error rather than masking
                // it as a clean EOF — the public contract is `Ok(None)`
                // means "stream cleanly drained", `Err(_)` means
                // "malformed".
                if !self.buffer.is_empty() {
                    let buffered = std::mem::take(&mut self.buffer);
                    let err = serde_json::from_slice::<T>(&buffered)
                        .err()
                        .map(anyhow::Error::from)
                        .unwrap_or_else(|| anyhow::anyhow!("plugin closed mid-frame"));
                    return Err(err);
                }
                return Ok(None);
            }
            self.buffer.extend_from_slice(&chunk[..n]);
        }
    }

    pub async fn write_message<T>(&mut self, message: &T) -> Result<()>
    where
        T: Serialize,
    {
        let mut line = serde_json::to_string(message)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }
}
