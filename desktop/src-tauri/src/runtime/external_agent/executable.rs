//! Shared executable resolution for Settings probes and frozen delegation targets.
//!
//! npm installs both a Unix shell shim and a Windows command shim under the same
//! bare name. Windows PATH lookup must use PATHEXT rather than selecting the
//! extensionless shell script, which CreateProcess rejects with OS error 193.

use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub(super) fn resolve_executable(command: &str) -> Result<PathBuf, String> {
    if command.trim().is_empty() || command.contains('\0') {
        return Err("external-agent executable cannot be empty".into());
    }
    let path = Path::new(command);
    if path.is_absolute() {
        return canonical_file(path);
    }
    if path.components().count() != 1 {
        return Err(
            "external-agent executable must be absolute or one bare command name from PATH".into(),
        );
    }
    let search = env::var_os("PATH").ok_or_else(|| {
        "PATH is unavailable; configure an absolute external-agent executable".to_owned()
    })?;
    let extensions = env::var_os("PATHEXT");
    resolve_in_path(command, &search, extensions.as_deref())
}

/// Keep PATH directory order and PATHEXT order, and never search a relative
/// directory. Explicit filenames and absolute paths remain exact user choices.
fn resolve_in_path(
    command: &str,
    search: &OsStr,
    extensions: Option<&OsStr>,
) -> Result<PathBuf, String> {
    for directory in env::split_paths(search).filter(|path| path.is_absolute()) {
        for candidate in executable_candidates(&directory, command, extensions) {
            if candidate.is_file() {
                return canonical_file(&candidate);
            }
        }
    }
    Err(format!(
        "external-agent executable '{command}' was not found; configure its absolute path"
    ))
}

fn executable_candidates(
    directory: &Path,
    command: &str,
    extensions: Option<&OsStr>,
) -> Vec<PathBuf> {
    if !cfg!(windows) || Path::new(command).extension().is_some() {
        return vec![directory.join(command)];
    }
    let extensions = extensions
        .unwrap_or_else(|| OsStr::new(".COM;.EXE;.BAT;.CMD"))
        .to_string_lossy();
    extensions
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(|extension| directory.join(format!("{command}{extension}")))
        .collect()
}

fn canonical_file(path: &Path) -> Result<PathBuf, String> {
    let canonical = dunce::canonicalize(path)
        .map_err(|_| "external-agent executable could not be resolved".to_owned())?;
    if !canonical.is_file() {
        return Err("external-agent executable is not a regular file".into());
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn explicit_filename_and_absolute_executable_remain_exact() {
        let executable = env::current_exe().expect("test executable");
        let canonical = dunce::canonicalize(&executable).expect("canonical executable");
        assert_eq!(
            resolve_executable(executable.to_str().expect("UTF-8 executable")).unwrap(),
            canonical
        );
        let search = env::join_paths([executable.parent().unwrap()]).unwrap();
        assert_eq!(
            resolve_in_path(
                executable.file_name().unwrap().to_str().unwrap(),
                &search,
                None
            )
            .unwrap(),
            canonical
        );
        assert!(resolve_executable("nested/tool").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_npm_command_shim_and_preserves_search_order() {
        let root = TempDir::new().unwrap();
        let first = root.path().join("npm with spaces");
        let second = root.path().join("native");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("codex"), "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::write(first.join("codex.cmd"), "@echo off\r\necho ready\r\n").unwrap();
        std::fs::write(second.join("codex.exe"), "later executable").unwrap();
        let search = env::join_paths([&first, &second]).unwrap();
        let resolved = resolve_in_path("codex", &search, Some(OsStr::new(".EXE;.CMD")))
            .expect("Windows launcher beside Unix shim");
        assert_eq!(
            resolved,
            dunce::canonicalize(first.join("codex.cmd")).unwrap()
        );
        let mut command = std::process::Command::new(&resolved);
        command.stdout(std::process::Stdio::piped());
        let child = aworkit_process::command::spawn_background_group(&mut command)
            .expect("resolved canonical batch path starts in a process group");
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "ready");

        std::fs::write(first.join("codex.exe"), "first executable").unwrap();
        assert_eq!(
            resolve_in_path("codex", &search, Some(OsStr::new(".EXE;.CMD"))).unwrap(),
            dunce::canonicalize(first.join("codex.exe")).unwrap()
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_keeps_extensionless_commands() {
        let root = TempDir::new().unwrap();
        let executable = root.path().join("codex");
        std::fs::write(&executable, "#!/bin/sh\n").unwrap();
        let search = env::join_paths([root.path()]).unwrap();
        assert_eq!(
            resolve_in_path("codex", &search, Some(OsStr::new(".EXE;.CMD"))).unwrap(),
            dunce::canonicalize(executable).unwrap()
        );
    }
}
