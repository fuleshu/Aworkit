//! Host shell dialect and child-only environment. No process-global PATH changes.
use std::{collections::BTreeMap, path::Path, process::Command};

fn dialect(program: &Path) -> String {
    program
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Describe the executable actually frozen for this Agent, including read-only date syntax.
pub fn context(program: &Path) -> String {
    let syntax = match dialect(program).as_str() {
        "cmd" => {
            "Windows cmd.exe syntax. Use date /t or echo %DATE% %TIME% to read the clock; bare date/time can prompt to change it. Unix date -u is unsupported. PowerShell is available through powershell -NoProfile -NonInteractive -Command when installed."
        }
        "powershell" | "pwsh" => {
            "PowerShell syntax. Use Get-Date to read the clock; use $env:NAME for environment variables."
        }
        _ => {
            "POSIX shell syntax. Use date or date -u to read the clock; use $NAME for environment variables."
        }
    };
    format!(
        "Host OS: {}. Shell executable: {}. {syntax}",
        std::env::consts::OS,
        program.display()
    )
}

/// Give host-shell descendants normal executable discovery without inheriting secrets.
/// Explicit invocation overrides win; standard Windows folders supplement the inherited PATH.
pub(crate) fn environment(explicit: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut environment = explicit.clone();
    #[cfg(windows)]
    {
        if !environment
            .keys()
            .any(|key| key.eq_ignore_ascii_case("PATH"))
        {
            let mut paths: Vec<_> = std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default();
            if let Some(root) = std::env::var_os("SystemRoot") {
                let root = std::path::PathBuf::from(root);
                for path in [
                    root.join("System32"),
                    root.join("System32/WindowsPowerShell/v1.0"),
                    root,
                ] {
                    if !paths.iter().any(|entry| {
                        entry
                            .to_string_lossy()
                            .eq_ignore_ascii_case(&path.to_string_lossy())
                    }) {
                        paths.push(path);
                    }
                }
            }
            if let Ok(path) = std::env::join_paths(paths) {
                environment.insert("PATH".into(), path.to_string_lossy().into_owned());
            }
        }
        if !environment
            .keys()
            .any(|key| key.eq_ignore_ascii_case("PATHEXT"))
        {
            environment.insert("PATHEXT".into(), ".COM;.EXE;.BAT;.CMD".into());
        }
    }
    environment
}

/// cmd.exe parses command text itself, rather than the C runtime argv convention.
/// /S removes our outer quotes; inner quotes, percent expansion and operators stay verbatim.
pub(crate) fn command_arguments(command: &mut Command, program: &Path, arguments: &[String]) {
    #[cfg(windows)]
    if dialect(program) == "cmd" && arguments.len() == 4 && arguments[..3] == ["/D", "/S", "/C"] {
        use std::os::windows::process::CommandExt;
        command
            .args(&arguments[..3])
            .raw_arg(format!("\"{}\"", arguments[3]));
        return;
    }
    command.args(arguments);
}
