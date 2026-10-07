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

/// The Windows variables Windows defines for a process because they describe
/// the *machine*, not the user.
///
/// A controlled child starts from an empty environment, and cmd.exe leaves an
/// undefined `%NAME%` reference verbatim instead of expanding it. An ordinary
/// command such as `mkdir "%SystemDrive%\Temp"` then resolves to a *relative*
/// path and silently creates a literal `%SystemDrive%` folder in the working
/// directory. Supplying the machine-scoped baseline keeps `%NAME%` references
/// meaningful while still inheriting no user environment, no credentials and no
/// Aworkit state.
#[cfg(windows)]
pub(crate) const WINDOWS_MACHINE_ENVIRONMENT: [&str; 15] = [
    // Volumes, the OS directory and the command processor.
    "SystemDrive",
    "SystemRoot",
    "windir",
    "ComSpec",
    // Machine-wide program and data directories, including the documented
    // architecture-suffixed names of the 32-bit views.
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    // Non-secret operating-system facts ordinary tools read.
    "OS",
    "PATHEXT",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "NUMBER_OF_PROCESSORS",
];

/// Adds the machine-scoped Windows baseline to one launch. A caller's explicit
/// environment is applied afterwards, so a configured value always wins.
#[cfg(windows)]
pub(crate) fn apply_machine_environment(command: &mut Command) {
    for name in WINDOWS_MACHINE_ENVIRONMENT {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

/// The per-user runtime locations ordinary tooling needs, without inheriting
/// credentials or Aworkit state.
///
/// These are the user-scoped counterpart of [`WINDOWS_MACHINE_ENVIRONMENT`]:
/// Windows defines them because they describe the *user* the process runs as. A
/// host-shell child that lacks them cannot run a build - `link.exe` resolves its
/// scratch directory through `TMP`/`TEMP` and falls back to the Windows
/// directory, which an ordinary user cannot write to - and cmd.exe leaves an
/// undefined `%USERPROFILE%` reference verbatim, exactly like the machine
/// references the baseline already protects. They are directory names, never
/// secrets.
#[cfg(windows)]
const WINDOWS_USER_ENVIRONMENT: [&str; 7] = [
    // The per-user scratch directory every compiler, linker and installer uses.
    "TEMP",
    "TMP",
    // The profile and per-user application-data directories Windows defines.
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
];

/// The POSIX counterparts of [`WINDOWS_USER_ENVIRONMENT`].
#[cfg(not(windows))]
const POSIX_USER_ENVIRONMENT: [&str; 4] = ["HOME", "TMPDIR", "TMP", "TEMP"];

/// Adds the per-user runtime baseline a caller did not already set. An explicit
/// invocation value always wins, and a launcher that defines no temp variable at
/// all still gets a usable per-user scratch directory rather than the
/// unwritable OS directory.
fn apply_user_runtime_environment(environment: &mut BTreeMap<String, String>) {
    #[cfg(windows)]
    let names: &[&str] = &WINDOWS_USER_ENVIRONMENT;
    #[cfg(not(windows))]
    let names: &[&str] = &POSIX_USER_ENVIRONMENT;
    let defined = |environment: &BTreeMap<String, String>, name: &str| {
        environment.keys().any(|key| key.eq_ignore_ascii_case(name))
    };
    for name in names {
        if !defined(environment, name) {
            if let Some(value) = std::env::var_os(name) {
                environment.insert((*name).to_owned(), value.to_string_lossy().into_owned());
            }
        }
    }
    for name in ["TEMP", "TMP"] {
        if !defined(environment, name) {
            let temp = std::env::temp_dir();
            environment.insert(name.to_owned(), temp.to_string_lossy().into_owned());
        }
    }
}

/// A caller's explicit environment plus the per-user runtime baseline.
///
/// Use this for a tool child that already receives executable discovery from
/// the launch path, so it needs the user runtime locations but not PATH.
pub(crate) fn user_runtime_environment(
    explicit: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut environment = explicit.clone();
    apply_user_runtime_environment(&mut environment);
    environment
}

/// Give host-shell descendants normal executable discovery and the per-user
/// runtime baseline without inheriting secrets.
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
    apply_user_runtime_environment(&mut environment);
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
