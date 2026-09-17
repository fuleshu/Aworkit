# Managed shell and Python jobs

Enable **Start shell job** or **Start Python job**, plus **Read job output**,
**Send job input**, **Stop job**, and **List jobs** in Settings and bind them to the
Agent. Add **Keep job running** when the Agent may deliberately leave a service
available. Start a new Chat: existing Chats keep their frozen tool selections and
permissions.

The tools work with a saved project or the Chat's private workspace.

| Tool | Behavior |
| --- | --- |
| `shell_start` | Starts the command and returns a durable job ID immediately. Stdin is a pipe by default; `interactive:false` closes it. |
| `shell` | With all four job controls bound, waits up to the configured initial wait (at most 10 seconds), then returns output and a job ID if still running. Without those controls, it retains bounded legacy execution. |
| `python_start` | Starts a script in the configured Python interpreter and returns a job ID immediately. Stdin is a pipe by default; `interactive:false` closes it. |
| `python` | With all four job controls bound, uses the same soft initial wait as `shell`. Otherwise retains its legacy hard timeout. Stdin is closed; use `python_start` for interactive scripts. |
| `job_output` | Returns status and new output. `waitMs` is a soft wait, at most 60 seconds. A wait expiration never kills the process. |
| `job_input` | Queues up to 16 KiB of exact stdin text. Include newlines when needed; `closeStdin:true` closes input after queued data. Inspect output to confirm the program processed it. |
| `job_list` | Lists only this Chat's jobs, including retained and uncollected jobs. |
| `job_stop` | Requests termination of the entire owned process tree, then returns output and cleanup evidence. |
| `job_keep` | Explicitly retains a running job, with a reason and normal approval policy. Report its ID to the user. Lifetime ends when stopped or Aworkit exits. |

Managed Python uses `-I -u`: interpreter isolation is retained and stdout/stderr
are unbuffered, so ordinary `print()` progress is visible before the script exits.
The interpreter is frozen per Chat, including a custom executable path. This is
host execution, with the same filesystem and network access as the desktop user.

Job IDs cannot control or read another Chat's processes. Launch, stdin and retention
use the normal frozen tool authority. Retention does not create a standing project
approval grant. Output collection and stopping an owned job do not require approval
unless the tool's approval policy is explicitly overridden.

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
tests, and `desktop/scripts/native-shell-jobs-smoke.mjs` (also run with `--python`) exercise real Windows
processes through an isolated native Chat profile. No Adashi QA-job behavior is
changed by this feature.
