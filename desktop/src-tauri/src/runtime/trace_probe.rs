//! TEMPORARY diagnostic probe for the wedged agent pass (task #185).
//!
//! A GUI release build has no console, so every marker appends one line to
//! `%TEMP%\aworkit-pass-trace.log`. The file is truncated on the first mark of a
//! process and capped, so a runaway loop cannot fill the disk. Remove this
//! module and every `trace_probe::mark` call once the fault is fixed.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Bound on one trace file, so a spinning loop cannot grow it without limit.
const MAXIMUM_TRACE_BYTES: u64 = 512 * 1024;

static STARTED: AtomicBool = AtomicBool::new(false);
static WRITTEN: AtomicU64 = AtomicU64::new(0);

/// Appends one stage marker. Never panics and never blocks on failure.
pub fn mark(stage: &str) {
    if WRITTEN.load(Ordering::Relaxed) > MAXIMUM_TRACE_BYTES {
        return;
    }
    let path = std::env::temp_dir().join("aworkit-pass-trace.log");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    if !STARTED.swap(true, Ordering::SeqCst) {
        options.truncate(true).write(true);
    }
    let Ok(mut file) = options.open(path) else {
        return;
    };
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let line = format!("{millis} {stage}\n");
    if file.write_all(line.as_bytes()).is_ok() {
        WRITTEN.fetch_add(line.len() as u64, Ordering::Relaxed);
    }
    let _ = file.flush();
}
