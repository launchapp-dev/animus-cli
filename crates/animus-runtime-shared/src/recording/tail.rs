//! Race-safe tail reader for daemon-restart gap reconstruction.
//!
//! When the daemon dies and restarts while a runner keeps appending to
//! `decisions.jsonl`, the daemon needs to "catch up" on events that landed
//! during the gap. This module provides [`DecisionTailReader`]: a stateful
//! reader that tracks a byte offset, reads only newly-appended bytes, and
//! handles the writer/reader race (the runner may be in the middle of
//! writing a line when the daemon tail-reads) by re-checking the file size
//! and only returning lines that ended in `\n` before the size snapshot.
//!
//! The tail reader does NOT mutate the file. The runner remains the sole
//! writer; the daemon is a pure consumer.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::DecisionEvent;

/// One unit of forward progress through `decisions.jsonl`.
#[derive(Debug, Clone)]
pub struct TailBatch {
    /// Events newly observed since the prior call.
    pub events: Vec<DecisionEvent>,
    /// The byte offset the reader is now sitting at. The next call to
    /// [`DecisionTailReader::read_new`] resumes from this offset.
    pub offset: u64,
    /// `true` when the reader observed a non-newline-terminated tail
    /// (likely the writer in the middle of appending a line). Callers can
    /// use this as a hint that another tail call will be productive.
    pub partial_tail: bool,
}

/// Stateful, race-safe tail reader for `decisions.jsonl`.
///
/// Two race patterns are handled:
/// 1. The writer is mid-append when the daemon calls [`Self::read_new`]:
///    the new bytes form an incomplete line. We detect that by checking
///    whether the buffer ends in `\n`. The trailing partial line is left
///    on disk and re-read on the next call.
/// 2. The file size shrinks (eg an archival rename + recreate). This is
///    surfaced as an error so the caller can decide to reset state.
pub struct DecisionTailReader {
    path: PathBuf,
    offset: u64,
    pending_partial: Vec<u8>,
}

impl DecisionTailReader {
    /// Open at byte offset `offset`. Pass `0` to read from the beginning.
    pub fn open(path: impl Into<PathBuf>, offset: u64) -> Self {
        Self { path: path.into(), offset, pending_partial: Vec::new() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Drain newly-appended events. Returns an empty batch when no progress
    /// is possible (file shorter than offset is treated as a hard error).
    ///
    /// Invariant: `self.offset` always points at the byte AFTER the last
    /// fully-parsed-and-emitted newline. `self.pending_partial` holds the
    /// raw bytes (no newline) that exist on disk in `[offset, offset + len)`
    /// and have NOT yet been emitted; they are part of an in-flight line
    /// the writer hasn't finished. On the next call we re-read starting at
    /// `self.offset + self.pending_partial.len()` so we never re-consume
    /// the partial bytes.
    pub fn read_new(&mut self) -> Result<TailBatch> {
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(TailBatch { events: Vec::new(), offset: self.offset, partial_tail: false });
            }
            Err(err) => return Err(err).with_context(|| format!("open {}", self.path.display())),
        };
        let len = file.metadata().context("stat decision log")?.len();
        let read_floor = self.offset + self.pending_partial.len() as u64;
        if len < read_floor {
            anyhow::bail!(
                "decision log {} shrank from offset {} (+ {} partial bytes) to length {} (writer reset?)",
                self.path.display(),
                self.offset,
                self.pending_partial.len(),
                len
            );
        }
        if len == read_floor {
            let partial_tail = !self.pending_partial.is_empty();
            return Ok(TailBatch { events: Vec::new(), offset: self.offset, partial_tail });
        }
        file.seek(SeekFrom::Start(read_floor))
            .with_context(|| format!("seek {} to {}", self.path.display(), read_floor))?;
        let to_read = (len - read_floor) as usize;
        let mut new_bytes = Vec::with_capacity(to_read);
        let mut reader = file.take(to_read as u64);
        reader.read_to_end(&mut new_bytes).context("tail-read decision log")?;
        // Prepend any partial bytes the prior read couldn't consume.
        let mut buffer = std::mem::take(&mut self.pending_partial);
        buffer.extend_from_slice(&new_bytes);
        // Split on `\n`. The trailing element after the last `\n` is held
        // back as a partial; the writer may still be appending to it.
        let mut consumed_bytes = 0usize;
        let mut events = Vec::new();
        for line in buffer.split_inclusive(|b| *b == b'\n') {
            let ends_with_nl = line.last() == Some(&b'\n');
            if !ends_with_nl {
                // Partial trailing line. Save the bytes for the next call,
                // and DO NOT advance the offset past them.
                self.pending_partial = line.to_vec();
                break;
            }
            // A complete line. Trim the newline for parsing.
            let line_no_nl = &line[..line.len() - 1];
            consumed_bytes += line.len();
            if line_no_nl.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            match serde_json::from_slice::<DecisionEvent>(line_no_nl) {
                Ok(event) => events.push(event),
                Err(err) => {
                    anyhow::bail!("tail-read corrupted decision-log entry in {}: {}", self.path.display(), err);
                }
            }
        }
        let new_offset = self.offset + consumed_bytes as u64;
        let partial_tail = !self.pending_partial.is_empty();
        self.offset = new_offset;
        Ok(TailBatch { events, offset: new_offset, partial_tail })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::recording::{Durability, Recorder};
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use tempfile::TempDir;

    fn write_full_events(path: &Path, n: usize) {
        let recorder = Recorder::create_with_durability(path, Durability::FsyncPerEvent).expect("recorder");
        for i in 0..n {
            recorder.record(&DecisionEvent::response_chunk("stdout", format!("event-{i}"))).expect("record");
        }
    }

    #[test]
    fn tail_reads_all_events_from_zero_offset() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("decisions.jsonl");
        write_full_events(&path, 5);
        let mut reader = DecisionTailReader::open(&path, 0);
        let batch = reader.read_new().expect("read");
        assert_eq!(batch.events.len(), 5);
        assert!(!batch.partial_tail);
        assert_eq!(batch.offset, reader.offset());
    }

    #[test]
    fn tail_returns_empty_when_no_new_bytes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("decisions.jsonl");
        write_full_events(&path, 2);
        let mut reader = DecisionTailReader::open(&path, 0);
        let _ = reader.read_new().expect("first read");
        let batch = reader.read_new().expect("second read");
        assert!(batch.events.is_empty());
    }

    #[test]
    fn tail_handles_partial_line_at_end_then_finishes_after_newline() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("decisions.jsonl");
        write_full_events(&path, 1);
        // Append a partial line (no newline) to simulate the writer mid-append.
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(br#"{"kind":"response_chunk","timestamp_ms":1,"stream":"stdout","text":"in-flight"#).unwrap();
        }
        let mut reader = DecisionTailReader::open(&path, 0);
        let batch1 = reader.read_new().expect("read partial");
        assert_eq!(batch1.events.len(), 1, "only the complete line is yielded");
        assert!(batch1.partial_tail, "the unfinished trailing line is detected");
        // Writer completes the line.
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"\"}\n").unwrap();
        }
        // The partial line we appended above is:
        //   {"kind":"response_chunk","timestamp_ms":1,"stream":"stdout","text":"in-flight
        // Adding `"}` plus newline closes the string + object.
        let batch2 = reader.read_new().expect("read after completion");
        assert_eq!(batch2.events.len(), 1);
        assert!(!batch2.partial_tail);
        let DecisionEvent::ResponseChunk { text, .. } = &batch2.events[0] else {
            panic!("wrong event kind");
        };
        assert_eq!(text, "in-flight");
    }

    #[test]
    fn tail_advances_offset_across_calls() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("decisions.jsonl");
        write_full_events(&path, 3);
        let mut reader = DecisionTailReader::open(&path, 0);
        let batch1 = reader.read_new().expect("read 1");
        assert_eq!(batch1.events.len(), 3);
        // Append more events with a fresh recorder writing to the same path.
        write_full_events(&path, 4);
        // The recorder above wrote 4 NEW events (append-only), totalling 7.
        let batch2 = reader.read_new().expect("read 2");
        assert_eq!(batch2.events.len(), 4);
    }

    #[test]
    fn tail_errors_on_shrunk_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("decisions.jsonl");
        write_full_events(&path, 3);
        let mut reader = DecisionTailReader::open(&path, 0);
        let _ = reader.read_new().expect("first read");
        // Truncate the file under the reader.
        std::fs::OpenOptions::new().write(true).truncate(true).mode(0o600).open(&path).unwrap();
        let err = reader.read_new().expect_err("shrunk file must error");
        assert!(err.to_string().contains("shrank"), "unexpected: {err}");
    }

    #[test]
    fn tail_is_noop_when_file_absent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("decisions.jsonl");
        let mut reader = DecisionTailReader::open(&path, 0);
        let batch = reader.read_new().expect("no file is ok");
        assert!(batch.events.is_empty());
        assert_eq!(batch.offset, 0);
    }

    #[test]
    fn gap_reconstruction_yields_only_post_gap_events_for_offset_aware_reader() {
        // Simulates the daemon-gap test: write 10 events, daemon read 3,
        // restart daemon at offset 3, append more events, see events 4..10
        // come in.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("decisions.jsonl");
        write_full_events(&path, 3);

        // First daemon: reads 3 events from offset 0.
        let mut daemon_a = DecisionTailReader::open(&path, 0);
        let batch_a = daemon_a.read_new().expect("daemon A read");
        assert_eq!(batch_a.events.len(), 3);
        let gap_offset = batch_a.offset;

        // Runner appends 7 more events DURING the gap.
        write_full_events(&path, 7);

        // Second daemon attaches at gap_offset; sees only events 4..10.
        let mut daemon_b = DecisionTailReader::open(&path, gap_offset);
        let batch_b = daemon_b.read_new().expect("daemon B read");
        assert_eq!(batch_b.events.len(), 7, "exactly the gap events");
    }
}
