//! Shared streaming JSONL reader for file-based Adapters.
//!
//! One reusable buffer, one line at a time — a Capture body is never read
//! whole into memory (single native files reach ~76 MB). A malformed line
//! warns and is skipped, never aborting the file: vendor stores contain
//! truncated tails and stray bytes, and losing one line must not lose the
//! Session. Only real I/O errors abort.

use std::io::{BufRead, BufReader};

use anyhow::{Context, Result};
use serde_json::Value;

use super::Capture;

/// Streaming reader yielding one parsed JSON record per line.
pub struct JsonlReader<R: BufRead> {
    reader: R,
    buf: Vec<u8>,
    line_no: u64,
    /// Where the lines come from, for warnings (a path, typically).
    context: String,
}

impl<R: BufRead> JsonlReader<R> {
    pub fn new(reader: R, context: impl Into<String>) -> Self {
        Self {
            reader,
            buf: Vec::new(),
            line_no: 0,
            context: context.into(),
        }
    }

    /// The next parseable record, skipping blank and malformed lines (with a
    /// warning). `Ok(None)` at end of input; `Err` only on real I/O errors.
    pub fn next_record(&mut self) -> Result<Option<Value>> {
        loop {
            if self.fill_line()?.is_none() {
                return Ok(None);
            }
            let line = trim_ascii(&self.buf);
            match serde_json::from_slice::<Value>(line) {
                Ok(record) => return Ok(Some(record)),
                Err(_) => self.warn_malformed(),
            }
        }
    }

    /// The next non-blank line, raw and trimmed — for Adapters that parse
    /// lines into typed structs borrowing from the buffer (bounded memory on
    /// multi-MB lines: skipped fields are never materialized). The caller
    /// owns parse failures: warn via [`Self::warn_malformed`] and continue,
    /// never abort the file.
    pub fn next_raw(&mut self) -> Result<Option<&[u8]>> {
        if self.fill_line()?.is_none() {
            return Ok(None);
        }
        // NLL limitation: returning a borrow of self.buf from a helper call
        // extends the loan; re-slice locally instead.
        Ok(Some(trim_ascii(&self.buf)))
    }

    /// Warn (once) that the line just returned failed to parse.
    pub fn warn_malformed(&self) {
        tracing::warn!(
            context = %self.context,
            line = self.line_no,
            "skipping malformed JSONL line"
        );
    }

    /// Read the next non-blank line into the buffer. `Ok(None)` at EOF.
    fn fill_line(&mut self) -> Result<Option<()>> {
        loop {
            self.buf.clear();
            let n = self
                .reader
                .read_until(b'\n', &mut self.buf)
                .with_context(|| format!("reading {}", self.context))?;
            if n == 0 {
                return Ok(None);
            }
            self.line_no += 1;
            if !trim_ascii(&self.buf).is_empty() {
                return Ok(Some(()));
            }
        }
    }
}

/// Open a zstd-compressed Capture body as a streaming JSONL reader.
pub fn open_capture(
    capture: &Capture,
) -> Result<JsonlReader<BufReader<zstd::stream::Decoder<'static, BufReader<std::fs::File>>>>> {
    let file = std::fs::File::open(&capture.body_path)
        .with_context(|| format!("opening capture {}", capture.body_path.display()))?;
    let decoder = zstd::stream::Decoder::new(file)?;
    Ok(JsonlReader::new(
        BufReader::new(decoder),
        capture.body_path.display().to_string(),
    ))
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |i| i + 1);
    &bytes[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(input: &str) -> Vec<Value> {
        let mut reader = JsonlReader::new(input.as_bytes(), "test");
        let mut out = Vec::new();
        while let Some(record) = reader.next_record().unwrap() {
            out.push(record);
        }
        out
    }

    #[test]
    fn yields_records_skipping_blank_and_malformed_lines() {
        let records = read_all("{\"a\":1}\n\n  \nnot json\n{\"b\":2}");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["a"], 1);
        assert_eq!(records[1]["b"], 2);
    }

    #[test]
    fn tolerates_missing_trailing_newline_and_invalid_utf8() {
        let mut input = b"{\"a\":1}\n\xff\xfe garbage\n{\"b\":2}".to_vec();
        let mut reader = JsonlReader::new(std::io::Cursor::new(std::mem::take(&mut input)), "test");
        let mut out = Vec::new();
        while let Some(record) = reader.next_record().unwrap() {
            out.push(record);
        }
        assert_eq!(out.len(), 2);
        assert_eq!(out[1]["b"], 2);
    }
}
