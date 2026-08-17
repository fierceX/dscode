// Shared JSONL file helpers used by conversation, artifact and usage stores.
//
// All three stores append independent JSON values and must tolerate a torn
// final record (process crash between write and newline). The policy is
// identical everywhere: a complete valid unterminated tail record is kept
// and gets a newline, while an invalid partial tail is truncated.
// Lossy readers skip invalid lines instead of failing whole-file reads.

use anyhow::Result;
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const SCAN_CHUNK_BYTES: u64 = 8 * 1024;

pub(crate) fn repair_unterminated_tail(path: &Path) -> Result<()> {
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(());
    }
    let mut scan_end = len;
    let mut tail_start = 0;
    while scan_end > 0 {
        let scan_start = scan_end.saturating_sub(SCAN_CHUNK_BYTES);
        let chunk_len = usize::try_from(scan_end - scan_start)?;
        let mut chunk = vec![0u8; chunk_len];
        file.seek(SeekFrom::Start(scan_start))?;
        file.read_exact(&mut chunk)?;
        if let Some(index) = chunk.iter().rposition(|byte| *byte == b'\n') {
            tail_start = scan_start + index as u64 + 1;
            break;
        }
        scan_end = scan_start;
    }
    let tail_len = usize::try_from(len - tail_start)?;
    let mut tail = vec![0u8; tail_len];
    file.seek(SeekFrom::Start(tail_start))?;
    file.read_exact(&mut tail)?;
    if serde_json::from_slice::<Value>(&tail).is_ok() {
        file.seek(SeekFrom::End(0))?;
        file.write_all(b"\n")?;
        file.flush()?;
    } else {
        file.set_len(tail_start)?;
    }
    Ok(())
}

pub(crate) fn append_line(path: &Path, line: &[u8], sync: bool) -> Result<()> {
    repair_unterminated_tail(path)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line)?;
    file.flush()?;
    if sync {
        file.sync_all()?;
    }
    Ok(())
}

pub(crate) fn parse_lossy_lines<F>(path: &Path, data: &str, warn: &mut F) -> Vec<Value>
where
    F: FnMut(String),
{
    let mut values = Vec::new();
    for (idx, line) in data.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(value) => values.push(value),
            Err(error) => warn(format!(
                "invalid JSONL in {} at line {} skipped by lossy read: {}",
                path.display(),
                idx + 1,
                error
            )),
        }
    }
    values
}
