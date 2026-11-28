//! A durable diagnostic trail for an abrupt exit.
//!
//! The recorded incident began with the app going blank and being restarted:
//! after the restart nothing said why the process died or how far it had got, so
//! the only recovery was the user typing "continue" and re-dispatching from
//! scratch. A panic now writes a bounded report naming the panic, where it
//! happened and the last committed sequence. The next start folds that report into
//! the recovery evidence, so the reason survives the restart.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

/// Where the report lives. Set once at application start.
static REPORT_PATH: OnceLock<PathBuf> = OnceLock::new();
/// The newest committed sequence this process observed, so a panic report can
/// say how far the history had got.
static LAST_COMMITTED_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A report is a diagnostic, never a payload: it is bounded before it is written.
pub(crate) const MAXIMUM_REPORT_BYTES: usize = 8 * 1024;

/// Remember how far the canonical history has got. Cheap, lock-free, and safe to
/// call from any thread.
pub fn note_committed_sequence(sequence: u64) {
    LAST_COMMITTED_SEQUENCE.fetch_max(sequence, Ordering::Relaxed);
}

/// Install the process panic hook and the report location.
///
/// The previous hook is preserved so the ordinary panic output still appears; a
/// diagnostic that hides the normal message would be worse than none.
pub fn install(path: PathBuf) {
    let _ = REPORT_PATH.set(path);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write_panic_report(info);
        previous(info);
    }));
}

/// The report left by the previous process, if it died abnormally.
pub fn last_report() -> Option<Value> {
    REPORT_PATH.get().and_then(|path| report_at(path))
}

/// Remove the report once it has been committed as recovery evidence, so it is
/// not repeated on every later recovery.
pub fn clear_last_report() {
    if let Some(path) = REPORT_PATH.get() {
        let _ = std::fs::remove_file(path);
    }
}

fn write_panic_report(info: &std::panic::PanicHookInfo<'_>) {
    let detail = info
        .payload()
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "the panic carried no message".to_owned());
    let location = info
        .location()
        .map(|location| format!("{}:{}", location.file(), location.line()));
    let thread = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_owned();
    if let Some(path) = REPORT_PATH.get() {
        let sequence = LAST_COMMITTED_SEQUENCE.load(Ordering::Relaxed);
        let _ = write_report_at(path, "panic", &detail, location.as_deref(), &thread, sequence);
    }
}

/// Write one bounded report atomically: a partial file would be misread as a
/// clean trail.
fn write_report_at(
    path: &Path,
    reason: &str,
    detail: &str,
    location: Option<&str>,
    thread: &str,
    last_committed_sequence: u64,
) -> Result<(), String> {
    let report = json!({
        "schemaVersion": 1,
        "reason": reason,
        "detail": bounded_single_line(detail),
        "location": location,
        "thread": bounded_single_line(thread),
        "lastCommittedSequence": last_committed_sequence,
        "observedAt": super::history::now_label(),
    });
    let body = bounded_single_line(&report.to_string());
    let temporary = path.with_extension("json.tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(&temporary, body).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}

/// Read one report, if a previous run left one.
fn report_at(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > MAXIMUM_REPORT_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

/// Collapse and bound one report field or the whole document.
fn bounded_single_line(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= MAXIMUM_REPORT_BYTES {
        return collapsed;
    }
    let mut end = MAXIMUM_REPORT_BYTES;
    while !collapsed.is_char_boundary(end) {
        end -= 1;
    }
    collapsed[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_report_names_the_reason_the_place_and_the_last_sequence() {
        let directory = std::env::temp_dir().join(format!(
            "aworkit-crash-report-{}-{}",
            std::process::id(),
            "panic"
        ));
        let path = directory.join("crash-report.json");
        write_report_at(
            &path,
            "panic",
            "index out of bounds: the len is 3 but the index is 9",
            Some("desktop/src-tauri/src/runtime/service.rs:412"),
            "aworkit-chat-worker",
            417,
        )
        .expect("write the report");

        let report = report_at(&path).expect("read the report back");
        assert_eq!(report["reason"], json!("panic"));
        assert!(
            report["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("index out of bounds"),
            "the panic text survives: {report}"
        );
        assert_eq!(report["lastCommittedSequence"], json!(417));
        assert_eq!(report["thread"], json!("aworkit-chat-worker"));
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_report_is_bounded_and_collapsed_to_one_line() {
        let directory = std::env::temp_dir().join(format!(
            "aworkit-crash-report-{}-{}",
            std::process::id(),
            "bounded"
        ));
        let path = directory.join("crash-report.json");
        write_report_at(
            &path,
            "panic",
            &format!("line one\nline two {}", "x".repeat(20_000)),
            None,
            "main",
            9,
        )
        .expect("write the report");
        let bytes = std::fs::read(&path).expect("read the raw report");
        assert!(
            bytes.len() <= MAXIMUM_REPORT_BYTES,
            "a report can never become a payload: {} bytes",
            bytes.len()
        );
        assert!(!bytes.contains(&b'\n'), "the report is one line");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_missing_report_is_not_an_error() {
        let path = std::env::temp_dir().join("aworkit-crash-report-does-not-exist.json");
        let _ = std::fs::remove_file(&path);
        assert!(report_at(&path).is_none());
    }
}
