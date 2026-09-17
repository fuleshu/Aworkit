# Tool execution investigation — 17 September 2026

## Scope and conclusion

Inspected the actual latest Aworkit chat, current Aworkit source, local
deepseek-harness source, official Codex documentation, and the persisted SplatMCP
QA results. The suspected lifecycle problem is real. It combines a confirmed
unbounded output-drain bug, missing model-visible background jobs, and a separate
MCP timeout mismatch. Some QA failures also belong to the test commands themselves.

The follow-up implementation now includes managed shell jobs, process-tree
supervision, and the final-response completion barrier, as well as the Windows
console-window fix. See [Managed shell jobs](shell-jobs.md) for the implemented
contract. The Adashi artifact `uml.tool_supervision.soft_wait_proposal` now records
the approved design, with linked tasks 89–91 (task IDs 90–92).
The Adashi QA/MCP timeout issue is explicitly excluded from this implementation.

## Evidence from the actual chat

The evidence and source comparison below describe the state at the initial
investigation, before the follow-up implementation. They are retained as the
diagnostic record; `shell-jobs.md` describes current behavior.

Source: read-only queries of `%APPDATA%/com.aworkit.desktop/runtime/history/aworkit.sqlite3`.
Chat: `chat.56f7feea0c9795ce653fc2c2128fe3be6919911b`.
Initial request: `implement adashi Task Id 10 in SplatMCP`.
The chat contained 12,530 events, 460 tool calls and 406 model-call spans.
This is newer than the previously investigated 8,114-event SplatMCP chat.

Durations below are differences between persisted tool-span start and end timestamps;
they include dispatch/result overhead and are not CPU-time measurements.

| Event sequences | Action | Elapsed | Recorded outcome |
| --- | --- | ---: | --- |
| 5411–5412 | `cargo test -p splatmcp-mcp --lib`, then read its redirected log | 684.324 s | Exited; 47 tests passed; test body reported 0.00 s |
| 10065–10066 | `tools/test_workspace.cmd`, then read its log | 421.949 s | Exited; success |
| 10362–10363 | `tools/test_workspace.cmd`, then display its log | 709.472 s | Exited; success |
| 6274–6275 | `start /b` test launch, short wait, read log | 70.338 s | Exited, code 1; log was in use |
| 6012–6013 | Adashi `run_jobs` | 32.16 s | Outcome uncertain |
| 8177–8178 | Adashi `run_jobs` | 33.056 s | Outcome uncertain |
| 10182–10183 | Adashi `run_jobs` | 35.168 s | Outcome uncertain |

There were 206 shell calls, totaling 4,151.1 seconds in tool spans (median 4.86 s).
Fifteen shell calls reported being killed by the 30-second limit. Several were
long builds/tests or explicit sleep/ping-based polling, including a background
installer launch followed by a 60-second wait. That wait caused a timeout of its
own process group. Shell arguments offered no job handle or per-call wait control.

At event 10381 the user instructed the agent to stop concerning itself with Adashi
QA and produce the installer. There were **zero subsequent Adashi QA calls** in
this chat. Thus the agent followed the instruction at the tool-call level; already
started server jobs had a separate lifecycle.

The trace does not record the exact desktop PID, pipe owners, or the moment the
user closed a window. It cannot prove which descendant caused each historical
stall. The native reproduction below proves that the current runner has the
matching failure mode, including ignoring cancellation after parent exit.

## Aworkit implementation findings

### Confirmed unbounded drain

`crates/aworkit-capability-host/src/process.rs`, `ProcessRunner::run_controlled`:

1. Starts the command in a process group/Windows Job Object and drains stdout and
   stderr on reader threads.
2. Checks deadline/cancellation only while polling the root child.
3. When that child exits, leaves the polling loop and calls blocking `join()` on
   both reader threads. Readers finish only at EOF.
4. Descendants retaining inherited pipe handles can keep EOF from arriving. No
   timeout or cancellation is checked while those joins wait.

This is not pipe-buffer saturation: readers are actively draining. It is waiting
for handles to close. `terminate_group` also gates group termination on root-child
liveness, which must be reconsidered when descendants outlive the root.

A finite native probe invoked the real `ProcessRunner` with this cmd command:

```bat
start /b powershell -NoProfile -NonInteractive -Command "Start-Sleep -Seconds 5; Write-Output descendant-finished" & echo parent-finished
```

Configured process timeout: 500 ms. A separate thread cancelled after one second.
Observed: `elapsed_ms=5289 termination=Exited cancelled=true cleanup=false`, with
both parent and descendant output. The descendant ended naturally after five
seconds; the deadline and cancellation did not interrupt the blocked drain.
The temporary probe source was removed after the experiment.

### No background-job contract in the model tool loop

`desktop/src-tauri/src/runtime/model_tool_loop.rs` invokes each tool synchronously
inside a `for call` loop and commits the completed exchange afterward. Its other
tool-loop variants have the same sequencing. Shell and Python dispatch return
settled output, not a live execution handle. The cancellation watcher in
`runtime/tool_loop.rs` is a cancellation path, not a soft-wait monitor that yields
partial results to the agent.

Concurrency between separate Chats is already implemented. It does not supply
background tools within one Chat. Running a tool on a thread does not help the
model if the calling loop immediately waits for that thread.

The current `grep_files` implementation uses up to eight scoped worker threads
and a cooperative 30-second wall-clock budget. It checks cancellation/deadlines
between work units and reads; it does not kill an isolated grep process. A blocked
I/O step can still delay cooperative cancellation.

### MCP request lifetime does not match QA job lifetime

`desktop/src-tauri/src/runtime/mcp.rs::production_peer_limits` fixes production
request timeouts at 30 seconds. The transport in
`crates/aworkit-capability-host/src/mcp/transport/peer.rs::execute_call` sets both
request and total timeout to that value. It collects progress in a registry but
returns that progress only after the response settles.

Adashi's current `src-tauri/src/qa.rs::run_jobs` records a run and executes the
selected jobs sequentially before returning the full run. Aworkit's request can
therefore expire while an Adashi job is still executing. Later `get_run` responses
in this chat proved the runs existed and were still running. The uncertain result
does not mean the QA job was stopped and must not trigger an automatic replay.

## Additional QA findings

Read-only inspection of `C:/src/SplatMCP/.adashi/adashi.sqlite3` confirmed:

- Run 2 lasted 20 minutes. Run 6 lasted 50 minutes: its workspace job timed out
  after 1,800,039 ms, then its Python job after 1,200,015 ms, then its E2E command
  failed quickly. These durations come from Adashi's server-side records.
- Jobs failed in 53–67 ms because quoted `tools/test_workspace.cmd` or
  `tools/python_e2e.cmd` command text was not recognized. These are command/quoting
  failures, not slow model inference.
- Run 4's Python job was marked passed with exit code 0, but its captured output
  included `scipy_and_pillow_recipes_run_through_the_same_executor ... FAILED`
  followed by `python_check: ok`. The captured evidence is internally inconsistent;
  the wrapper's exit propagation and shared/stale log handling need repair before
  trusting such a pass.
- Adashi's current timeout cleanup calls `child.kill()` on the direct process,
  not a supervised descendant tree. Its API exposes no QA cancellation operation.
  Stopping Aworkit's wait cannot guarantee cancellation of that remote work.

SplatMCP's current `app_launch.rs` also contains a nominal missing-executable test
that falls back to the real installed/adjacent app and may actually launch it.
This is a separate fixture-isolation concern. Its present launcher redirects
stdio to null, so this fact alone does not identify the pipe holder in the chat.

## Comparison

| Behavior | Aworkit now | deepseek-harness checkout | Codex evidence |
| --- | --- | --- | --- |
| Long shell execution | Blocking final result; hard kill on timeout | `run_in_background: true` returns job id immediately; background start has no executor timeout | Local `exec_command` yielded output and a live session after about 10 s; `write_stdin` collected completion |
| Later control | No model-facing shell job handle | `job_output`, `job_list`, `job_kill`; bounded waits do not cancel jobs | Session continuation/input; App Server documents streaming output and explicit termination |
| Completion delivery | Model waits for dispatch result | Notices to owner; bounded idle-owner wakeups; instructions discourage duplicate work and busy polling | Do not infer universal automatic wakeups from the shell continuation test |
| Every tool has soft-wait semantics | No | Background jobs are explicit; foreground bash still kills on timeout | No such universal guarantee; official MCP docs retain a per-tool timeout |

Deepseek source inspected:
`packages/shell/tool-bash/src/index.ts` (background switch and job registration),
`packages/jobs/tool-jobs/src/index.ts` (wait/read/kill and completion delivery),
`packages/jobs/jobs-local/src/index.ts` (owned job lifecycle), and
`docs/subsystems/shell.md` (incremental output and spill-file recovery).

Codex sources:
[App Server process/command execution](https://learn.chatgpt.com/docs/app-server)
documents process handles, streaming output, stdin and termination.
[Configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
documents unified exec and the separate default 60-second MCP tool timeout.
The local 12-second shell probe returned its start marker plus session id after
10.01 seconds, then `write_stdin` returned the completion marker and exit code 0.
The wait boundary did not terminate the command. This confirms this session's
behavior, not every Codex version or configuration.

## Recommended implementation, in order

This elaborates the proposal persisted in Adashi; it is not implemented here.

1. **Repair lifecycle supervision.** Keep deadline/cancellation monitoring alive
   through descendant exit, pipe draining and cleanup. Track root exit, remaining
   descendants and output completion separately. Use bounded drain/cleanup paths;
   do not merely detach a permanently blocked reader or kill only the parent.
2. **Introduce one supervised job contract for all tool invocations.** Extend the
   existing invocation broker, process runtime and process supervisor. Use async
   execution or a bounded blocking pool, with reserved monitor/control capacity.
   Use a process boundary for code that cannot be interrupted safely. A separate
   OS process for every tiny file read is unnecessary; unbounded unkillable threads
   are also not an adequate isolation mechanism.
3. **Separate soft wait from hard limits.** Initially wait roughly 10 seconds.
   If work continues, return job id, elapsed time, new output, actual state and
   supported controls. Let the owning model choose another bounded wait, inspect,
   do independent work, or explicitly stop. Do not kill shell work at this boundary.
   Retain explicit resource ceilings and user cancellation as separate policies.
4. **Preserve identity, output and conversation correctness.** Scope handles to
   Chat/invocation/generation; preserve frozen authority. Keep bounded output tails,
   cursor-based reads and full spill artifacts. A pending acknowledgement must not
   settle the underlying invocation. Each provider tool call still gets exactly
   one result; later collection is a separate call with separate correlation.
5. **Schedule model attention safely.** The monitor queues notices to the owning
   Chat; it does not invoke a second model concurrently into that Chat's exchange.
   Coalesce progress and completion notices, wake idle owners within a budget,
   accept user steering during waits, and avoid constant LLM polling. Permit
   independent tool jobs to overlap under explicit concurrency/resource policy;
   do not blindly parallelize conflicting mutations or tests sharing ports/logs.
6. **Support the real desktop/MCP test lifecycle.** Start the desktop and server as
   tracked jobs; wait for a readiness signal; run interactions as separate bounded
   steps; collect logs/screenshots/test exit status; then stop only owned processes.
   Allow stdio sessions/input where required, and distinguish a ready service from
   a completed test. App readiness must include the actual WebView/server path.
7. **Handle remote QA explicitly.** Retain the original MCP request across soft
   waits, expose any server progress, and make the transport deadline a distinct
   setting. For durable remote jobs, prefer start returning a run id plus status,
   incremental logs and cancel operations. Adashi would need that server-side
   extension for reliable cancellation. Transport loss remains uncertain until
   status reconciliation; never silently rerun a side-effecting job.

Acceptance cases should include a parent exiting with open descendant pipes,
silent GUI processes, a build longer than the first wait, completion while the
agent does other work, concurrent desktop/MCP interaction, stop during pipe drain,
MCP work beyond 30 seconds, disconnect/reconnect without replay, Chat isolation,
crash recovery, output truncation with cursor recovery, and accurate failed-test
exit propagation. Native Windows validation is essential.

## Implemented console-window fix and validation

The shared process runner now passes `CREATE_NO_WINDOW` to the command-group
builder. Setting it on `std::process::Command` alone would be overwritten by
`command-group` 5.0.1 when it adds `CREATE_SUSPENDED`. The fix suppresses the tool's
console allocation; deliberately launched GUI applications can still open their UI.

The native shell regression binary uses the Windows GUI subsystem, matching the
desktop host. Before the fix, `GetConsoleWindow()` returned nonzero (`6754560`)
and the regression failed. After the fix, direct PowerShell and cmd launching
PowerShell both returned zero. All four native shell tests passed, preserving
date syntax, nested quoting and installed-command discovery.

The Win32 probe requires full PowerShell language support; the sandbox's
constrained mode rejected `Add-Type`, so the native test was run through the
approved unsandboxed test invocation. Temporary compiler files were explicitly
scoped to a test directory. The existing running Aworkit binary has not been
replaced or restarted by this investigation; rebuilding it is needed to use the fix.
