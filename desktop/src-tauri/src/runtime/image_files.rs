//! One bounded local image file. Relative names use the frozen Chat workspace;
//! absolute names open an anchored parent directory, never a process-wide cwd.
use aworkit_capability_host::{CancellationToken, FileAuthority, ProjectFiles};
use std::path::{Component, Path};

pub(crate) fn read(
    files: &ProjectFiles,
    path: &Path,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, String> {
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(
            "Image paths cannot contain '..'; use the complete absolute file path instead.".into(),
        );
    }
    if !path.is_absolute() {
        return files
            .read_image_source_v1(path, cancellation)
            .map(|r| r.bytes)
            .map_err(|e| e.to_string());
    }
    #[cfg(windows)]
    if path.components().any(|part| matches!(part, Component::Prefix(prefix) if matches!(prefix.kind(), std::path::Prefix::DeviceNS(_) | std::path::Prefix::Verbatim(_)))) {
        return Err("Use an ordinary image file path, not a Windows device path.".into());
    }
    if cancellation.is_cancelled() {
        return Err("Image acquisition cancelled".into());
    }
    let parent = path.parent().ok_or("Image path has no parent directory")?;
    let name = path.file_name().ok_or("Image path must name a file")?;
    let directory = ProjectFiles::new(FileAuthority {
        root: parent.to_owned(),
        allow_write: false,
    })
    .map_err(|e| e.to_string())?;
    directory
        .read_image_source_v1(Path::new(name), cancellation)
        .map(|r| r.bytes)
        .map_err(|e| e.to_string())
}
