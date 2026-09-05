//! Browser processes release profile files after native window destruction.
//! Keep ownership of the exact temporary directory until bounded cleanup finishes.
use std::{path::Path, time::Duration};

pub(super) struct WebProfile(Option<tempfile::TempDir>);

impl WebProfile {
    pub(super) fn create() -> Result<Self, String> {
        tempfile::Builder::new()
            .prefix("aworkit-web-")
            .tempdir()
            .map(|directory| Self(Some(directory)))
            .map_err(|e| format!("renderer profile unavailable: {e}"))
    }

    pub(super) fn path(&self) -> &Path {
        self.0.as_ref().expect("live web profile").path()
    }
}

impl Drop for WebProfile {
    fn drop(&mut self) {
        let Some(directory) = self.0.take() else {
            return;
        };
        // These paths originate only from our TempDir, never from a page or model.
        // Retrying off the UI thread also lets WebView2 finish releasing its handles.
        std::thread::spawn(move || {
            for _ in 0..100 {
                match std::fs::remove_dir_all(directory.path()) {
                    Ok(()) => return,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
            eprintln!(
                "Aworkit could not remove a temporary web profile after browser shutdown: {}",
                directory.path().display()
            );
        });
    }
}
