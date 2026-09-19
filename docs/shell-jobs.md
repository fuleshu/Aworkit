# Managed shell and Python jobs

`shell`, `shell_start`, `python` and `python_start` all run as supervised jobs.
There is exactly one behaviour, in every Chat: a tool improvement replaces the
previous behaviour instead of adding a second mode, and no Chat is left on an
older blocking path because an unrelated switch stayed off.

Binding **Host shell** or **Host Python** is all that is needed. The control tools
that serve them — **Read job output**, **Send job input**, **Stop job**, **List
jobs**, **Keep job running** — accompany that capability, because reading, feeding
or stopping a process the Chat was already authorised to start is not new
authority. They are resolved when the pass runs, so a Chat that predates a tool
improvement adopts it in its next pass.

| Tool | Behavior |
| --- | --- |
| `shell` | Runs the command and yields after a soft wait of at most 10 seconds, returning partial output plus a job ID if it is still running. Stdin is closed; use `shell_start` for interactive commands. |
| `shell_start` | Starts the command and returns the job ID immediately. Stdin is a pipe by default; `interactive:false` closes it. |
| `python` | Runs the script in the configured interpreter with the same soft wait as `shell`. Stdin is closed; use `python_start` for interactive scripts. |
| `python_start` | Starts the script and returns the job ID immediately. Stdin is a pipe by default; `interactive:false` closes it. |
| `job_output` | Returns status and new output. `waitMs` is a soft wait, at most 60 seconds. A wait expiration never kills the process. |
| `job_input` | Queues up to 16 KiB of exact stdin text. Include newlines when needed; `closeStdin:true` closes input after queued data. Inspect output to confirm the program processed it. |
| `job_list` | Lists only this Chat's jobs, including retained and uncollected jobs. |
| `job_stop` | Requests termination of the entire owned process tree, then returns output and cleanup evidence. |
| `job_keep` | Explicitly retains a running job, with a reason and normal approval policy. Report its ID to the user. Lifetime ends when stopped or Aworkit exits. |

A job ends when it exits, is stopped, exceeds its output resource limit, or
Aworkit exits. A soft wait only returns a snapshot: it never kills work, and
"still running" is a normal result, not a failure. The `timeoutSeconds` value that
older Settings and Chat records still carry is no longer a deadline and no longer
bounds a command; it is retained so stored records keep decoding. A bounded
wall-clock deadline still applies where the caller is a one-shot probe, such as
the Settings adapter health check, which is not an interface of these tools.

Managed Python uses `-I -u`: interpreter isolation is retained and stdout/stderr
are unbuffered, so ordinary `print()` progress is visible before the script exits.
The interpreter is resolved per pass, including a custom executable path. This is
host execution, with the same filesystem and network access as the desktop user.

Job IDs cannot control or read another Chat's processes. Launch, stdin and
retention use the Chat's authority and approval policy; a capability outside that
authority is settled by the broker when it is called. Retention does not create a
standing project approval grant.

Before accepting a final response, the runtime checks for running jobs that were
not explicitly retained, and finished jobs whose output has not been collected.
It returns a reminder to the model to read, wait, stop or retain the job. The check
also applies after approval resume. Five consecutive attempts to ignore it fail
visibly and request cleanup of unretained jobs. User Stop bypasses this gate and
reaches job cleanup even while the model is waiting for approval.

Output is stored in per-job files, with independent byte cursors for stdout/stderr.
Each response is bounded to 256 KiB; default job reads return at most 64 KiB. Read
again if `moreOutput` is true. Passing a previous cursor re-reads from that position;
omitting it uses the last durably acknowledged position. Output is decoded as UTF-8
with replacement for invalid sequences; the files preserve original bytes.

There are at most eight running jobs per Chat and 32 in the application. A job
that reaches the 64 MiB output resource threshold is stopped; this is separate
from a soft wait. The monitor samples output, so a fast writer may exceed the
threshold before termination. At most 128 job records are retained; acknowledged
finished jobs and their output may be evicted when another job starts.

Windows shell and Python processes launch without a console window. GUI apps can still show their own
windows. Windows Job Objects track descendants even after the root process exits;
closing the application's job handles terminates their processes. Unix uses POSIX
process groups; deliberately detached processes that create a new session are
outside that boundary. Stdin interaction is not a terminal/PTY and does not supply
desktop automation or MCP-specific behavior.

After restart, old live jobs are shown as interrupted with an uncertain result.
Commands are never replayed to reconstruct a job, and an old PID is never used to
kill an unrelated new process. Captured output and acknowledged cursors remain
available. Check `treeEmpty` and `error` before claiming successful cleanup.

Verification: native process-session tests, the desktop job-registry and model-loop
tests, and `desktop/scripts/native-shell-jobs-smoke.mjs` (also run with `--python`)
exercise real Windows processes through an isolated native Chat profile. The smoke
run includes a Chat whose workflow never selected the control tools, to prove that
control is implied by the capability. No Adashi QA-job behavior is changed.
