//! Real Windows shell regressions: date dialect, nested quoting and installed-tool discovery.
#![cfg(windows)]
// Match Tauri's GUI process: a console-subsystem test parent can mask a popup.
#![windows_subsystem = "windows"]
use aworkit_capability_host::{
    BuiltInProcessTools, CancellationToken, HostToolLimitsV1, NativeProcessPort, ShellInvocationV1,
    ToolAuthorityModeV1,
};
use std::{collections::BTreeMap, path::PathBuf};

fn run(shell: PathBuf, command: &str) -> String {
    // Keep the helper hermetic: an explicit invocation environment still wins
    // over the per-user runtime baseline the shell tool now adds.
    let temporary = tempfile::tempdir().unwrap();
    let temporary_path = temporary.path().to_string_lossy().into_owned();
    run_with_environment(
        shell,
        command,
        BTreeMap::from([
            ("TEMP".into(), temporary_path.clone()),
            ("TMP".into(), temporary_path),
        ]),
    )
}

fn run_with_environment(
    shell: PathBuf,
    command: &str,
    environment: BTreeMap<String, String>,
) -> String {
    let result = BuiltInProcessTools::new(NativeProcessPort)
        .execute_shell(
            &ShellInvocationV1 {
                mode: ToolAuthorityModeV1::HostShell,
                shell_program: shell,
                command_text: command.into(),
                working_directory: None,
                environment,
                limits: HostToolLimitsV1::default(),
            },
            &CancellationToken::default(),
        )
        .unwrap();
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert_eq!(
        result.status,
        Some(0),
        "stdout: {stdout}; stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!result.output_truncated);
    stdout.into_owned()
}

fn system32() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32")
}

#[test]
fn cmd_date_read_is_noninteractive_and_quotes_survive() {
    let shell = system32().join("cmd.exe");
    let date = run(shell.clone(), "date /t");
    assert!(date.bytes().any(|b| b.is_ascii_digit()));
    assert_eq!(date.lines().count(), 1, "date /t must not prompt: {date}");
    let text = run(shell, "echo \"quoted text\" && echo second");
    assert_eq!(
        text.lines().map(str::trim).collect::<Vec<_>>(),
        ["\"quoted text\"", "second"]
    );
}

#[test]
fn cmd_finds_powershell_and_preserves_its_command_quotes() {
    let text = run(
        system32().join("cmd.exe"),
        r#"powershell -NoProfile -NonInteractive -Command "Get-Date -Format 'yyyy-MM-dd'; Write-Output 'quoted text'""#,
    );
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0].len(), 10);
    assert_eq!(lines[1], "quoted text");
}

#[test]
fn configured_powershell_can_read_date_and_call_installed_commands() {
    let text = run(
        system32().join("WindowsPowerShell/v1.0/powershell.exe"),
        "Get-Date -Format 'yyyy-MM-dd'; cmd /d /c echo nested-command-ok",
    );
    assert!(text.contains("nested-command-ok"), "{text}");
}

#[test]
fn cmd_expands_machine_windows_variables_instead_of_creating_literal_paths() {
    // A controlled child starts from an empty environment, and cmd.exe leaves an
    // undefined `%NAME%` reference verbatim. `mkdir "%SystemDrive%\Temp"` then
    // resolved to a *relative* path and silently created a literal
    // '%SystemDrive%' folder in the working directory instead of addressing the
    // volume it names.
    let names = [
        "SystemDrive",
        "SystemRoot",
        "windir",
        "ProgramData",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "CommonProgramFiles",
        "OS",
    ];
    let command = names
        .iter()
        .map(|name| format!("echo {name}=[%{name}%]"))
        .collect::<Vec<_>>()
        .join(" & ");
    let text = run(system32().join("cmd.exe"), &command);
    let reported: BTreeMap<&str, &str> = text
        .lines()
        .filter_map(|line| line.trim().split_once('='))
        .map(|(name, value)| (name, value.trim_matches(['[', ']'])))
        .collect();
    // Whatever the launching host defines among the machine baseline must reach
    // the child expanded; a literal reference is the defect this covers. A host
    // that defines less (a stripped launcher) can only be matched to what it has.
    assert!(
        std::env::var("SystemRoot").is_ok(),
        "the test host must define the Windows machine baseline"
    );
    for name in names {
        let value = *reported
            .get(name)
            .unwrap_or_else(|| panic!("{name} was not reported: {text}"));
        assert!(
            !value.contains('%'),
            "{name} stayed a literal reference in the child shell: {value}"
        );
        if let Ok(expected) = std::env::var(name) {
            assert_eq!(value, expected, "{name} must reach the child unchanged");
        }
    }
}

#[test]
fn host_shell_children_get_a_writable_user_temp_and_an_expanded_profile() {
    // The controlled child starts from an empty environment, so the shell tool
    // must supply the per-user runtime baseline itself. Without a real %TEMP% a
    // compiler or linker falls back to the unwritable Windows directory (rustc
    // reproduces `LNK1104: cannot open file 'C:\WINDOWS\lnk{...}.tmp'`), and
    // without %USERPROFILE% tools that resolve the user home fail.
    let text = run_with_environment(
        system32().join("cmd.exe"),
        "echo TEMP=[%TEMP%] & echo TMP=[%TMP%] & echo USERPROFILE=[%USERPROFILE%] \
         & echo aworkit-temp-probe> \"%TEMP%\\aworkit-temp-probe.txt\" \
         & type \"%TEMP%\\aworkit-temp-probe.txt\" \
         & del \"%TEMP%\\aworkit-temp-probe.txt\"",
        BTreeMap::new(),
    );
    let reported: BTreeMap<&str, &str> = text
        .lines()
        .filter_map(|line| line.trim().split_once('='))
        .map(|(name, value)| (name, value.trim_matches(['[', ']'])))
        .collect();
    for name in ["TEMP", "TMP"] {
        let value = *reported
            .get(name)
            .unwrap_or_else(|| panic!("{name} was not reported: {text}"));
        assert!(
            !value.contains('%'),
            "{name} stayed a literal reference in the child shell: {value}"
        );
    }
    assert!(
        PathBuf::from(reported["TEMP"]).is_dir(),
        "the child %TEMP% must be an existing directory: {text}"
    );
    assert!(
        text.contains("aworkit-temp-probe"),
        "the child could not create and read a file in %TEMP%: {text}"
    );
    // The profile is passed through rather than synthesized, so a stripped
    // launcher that defines none can only be matched to what it has.
    if let Ok(expected) = std::env::var("USERPROFILE") {
        let value = *reported
            .get("USERPROFILE")
            .unwrap_or_else(|| panic!("USERPROFILE was not reported: {text}"));
        assert!(
            !value.contains('%'),
            "%USERPROFILE% stayed a literal reference in the child shell: {value}"
        );
        assert_eq!(
            value, expected,
            "%USERPROFILE% must reach the child unchanged"
        );
    }
}

#[test]
fn host_shells_do_not_allocate_a_console_window() {
    // Query the child process itself: a hidden parent alone does not prevent
    // a GUI-hosted shell from allocating a new console when it is spawned.
    let probe = concat!(
        "Add-Type ('using System; using System.Runtime.InteropServices; ",
        "public static class ConsoleProbe { [DllImport(' + [char]34 + ",
        "'kernel32.dll' + [char]34 + ')] public static extern IntPtr GetConsoleWindow(); }'); ",
        "[ConsoleProbe]::GetConsoleWindow().ToInt64()",
    );
    let powershell = system32().join("WindowsPowerShell/v1.0/powershell.exe");
    assert_eq!(run(powershell, probe).trim(), "0");
    let nested = format!("powershell -NoProfile -NonInteractive -Command \"{probe}\"");
    assert_eq!(run(system32().join("cmd.exe"), &nested).trim(), "0");
}
