//! Image files dropped onto the window, imported natively.
//!
//! WebKitGTK 2.52 denies the page access to a dropped file's data: since the
//! CVE-2025-13947 hardening, `DataTransfer.allowsFileAccess()` is false on
//! every port except Cocoa, so a file dragged from the file manager arrives with
//! an empty `dataTransfer.files`/`items` (WebKit bug 323277) and the HTML5 drop
//! path has nothing to attach. The window's own drag-drop event still carries
//! the dropped paths, so the image is imported here — through the same native
//! store the file chooser uses — and the resulting attachments are announced to
//! the webview. No filesystem path ever reaches the renderer.
//!
//! This is the Linux path only: the platform configuration enables the window's
//! drag-drop handler there. On Windows the handler would take external drops
//! away from the page (wry #904) and break in-page drag, so the HTML5 drop path
//! stays in charge; the browser preview has no native runtime at all.
use aworkit_capability_host::model_images::ImageAttachmentV1;
use aworkit_desktop::runtime::ChatImageStore;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{Emitter, Manager, Window, WindowEvent};

/// Announced to the webview after a drop, whether it produced attachments or a
/// refusal to show the user.
pub const DROPPED_IMAGES_EVENT: &str = "aworkit:dropped-images";

/// What one drop produced.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedImagesV1 {
    /// Attachments already stored by the native image store.
    pub attachments: Vec<ImageAttachmentV1>,
    /// Why nothing was attached, when nothing was.
    pub reason: Option<String>,
}

/// Whether a dropped path names a file the image store is asked to import.
///
/// A drop can carry anything — folders, documents, a mix — so the extension
/// decides what is even offered to the store. The store's own sniffing stays
/// the authority on the actual bytes.
pub fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp"
            )
        })
        .unwrap_or(false)
}

/// Imports the image files of a drop and announces the result.
///
/// The window's drag-drop event is window-wide, so a drop lands on the Chat
/// workspace rather than on a specific element; the composer is the only
/// surface that accepts images, so that is where they go.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    let WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = event else {
        return;
    };
    if window.label() != "main" {
        return;
    }
    // The runtime (and with it the image store) may still be starting; a drop
    // that early is simply not offered anywhere to be stored.
    let Some(store) = window.app_handle().try_state::<ChatImageStore>() else {
        return;
    };
    let store = store.inner().clone();
    let offered: Vec<PathBuf> = paths.iter().filter(|path| is_image_path(path)).cloned().collect();
    let carried_a_file = !paths.is_empty();
    let window = window.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut attachments = Vec::with_capacity(offered.len());
        let mut reason = None;
        for path in offered {
            match store.import_path(&path) {
                Ok(attachment) => attachments.push(attachment),
                // The store's own reason is what the composer shows for a file
                // the picker would also refuse.
                Err(refusal) => reason = Some(refusal),
            }
        }
        if attachments.is_empty() && reason.is_none() && carried_a_file {
            reason = Some("Choose PNG, JPEG or WebP images.".to_owned());
        }
        if attachments.is_empty() && reason.is_none() {
            return;
        }
        let _ = window.emit(
            DROPPED_IMAGES_EVENT,
            DroppedImagesV1 {
                attachments,
                reason,
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_only_files_the_image_store_could_accept() {
        for accepted in [
            "/home/me/photo.png",
            "/home/me/PHOTO.PNG",
            "/home/me/scan.jpeg",
            "C:\\Pictures\\holiday.JPG",
            "/home/me/drawing.webp",
        ] {
            assert!(is_image_path(Path::new(accepted)), "{accepted}");
        }
        for refused in [
            "/home/me/notes.txt",
            "/home/me/archive.zip",
            "/home/me/photo.png.txt",
            "/home/me/photo",
            "/home/me/photos",
            "/home/me/.png",
        ] {
            assert!(!is_image_path(Path::new(refused)), "{refused}");
        }
    }
}
