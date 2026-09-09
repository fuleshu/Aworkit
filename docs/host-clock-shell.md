# Host clock and shell behavior

The 2026-09-08 date question used a Unix `date -u` command in Windows cmd, then failed to find `powershell` because the native process runner had removed PATH. The supplied clock was only an unlabeled Chat-start timestamp. The fixes preserve the existing tool selection and frozen executable authority.

Workspace Instructions now supplies a trusted host-clock reminder, including Chat start, latest committed user-turn UTC, local date/time with UTC offset, and weekday. It explicitly permits answering ordinary date/time questions from context. Each new user turn refreshes the reminder even if no guidance file changes; replay uses the admitted text, and compaction restores clock context with the selected instructions. The contribution reserves a fixed 1 KiB from its configured byte budget, so changing weekday lengths does not invalidate file-guidance state. Smaller budgets omit clock context. UTC is explicitly labeled when the OS local offset is unavailable.

Host commands preserve the application's PATH in their child environment. Host-shell invocations also supply PATHEXT and append missing standard Windows command/PowerShell folders. Explicit invocation environment entries take precedence. The parent environment and Windows PATH settings are never modified. Other application variables and credentials remain excluded unless explicitly supplied.

Agent context identifies its frozen shell executable and dialect. Windows cmd guidance uses `date /t` or `echo %DATE% %TIME%`; PowerShell uses `Get-Date`. cmd command text passes through its own `/D /S /C` quoting convention, preserving nested quotes and operators rather than applying C-runtime argument escaping.

Validation:

- `cargo test -p aworkit-capability-host --test host_shell --test host_network --test milestone_05`
- `cargo test --manifest-path desktop/src-tauri/Cargo.toml --lib workspace_instructions`
- `node scripts/native-clock-shell-smoke.mjs` from `desktop`, after rebuilding its frontend and native binary. This uses an isolated profile, the real workflow editor and Chat UI, a deterministic provider, and actual Windows shell processes. It verifies provider context and tool execution; it does not measure a production model's adherence to the guidance.

Verified on Windows on 2026-09-09: 12 host/process tests and 27 instruction/runtime tests passed. TypeScript and the Vite build passed. The native fixture completed five provider requests: one direct date response from context, plus a call and result for each of cmd and PowerShell. The broader desktop test command also encounters an unrelated existing compile error in `tests/mcp_probe.rs` (its `McpServerConfigurationV2` initializer lacks `plugin` and `tools`).

For native QA, explicitly clear any development URL in `TAURI_CONFIG` when compiling. If a running Aworkit instance locks `target/debug/aworkit-desktop.exe`, the newly compiled binary may still exist at `target/debug/deps/aworkit_desktop.exe`; set `AWORKIT_QA_BINARY` to that path. Close the running application before replacing its regular executable.
