//! Real Windows shell regressions: date dialect, nested quoting and installed-tool discovery.
#![cfg(windows)]
use aworkit_capability_host::{
    BuiltInProcessTools, CancellationToken, HostToolLimitsV1, NativeProcessPort, ShellInvocationV1,
    ToolAuthorityModeV1,
};
use std::{collections::BTreeMap, path::PathBuf};

fn run(shell: PathBuf, command: &str) -> String {
    let result = BuiltInProcessTools::new(NativeProcessPort)
        .execute_shell(
            &ShellInvocationV1 {
                mode: ToolAuthorityModeV1::HostShell,
                shell_program: shell,
                command_text: command.into(),
                working_directory: None,
                environment: BTreeMap::new(),
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
