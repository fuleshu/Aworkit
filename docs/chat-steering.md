# Steering a stopped Chat

Press **Stop response**, then send a message to steer the interrupted model node.
In Standard Agent, stopping the Agent and sending a correction continues that
Agent with the existing plan. Stopping the Plan node continues the Plan node.
After a response finishes normally, the next message starts a fresh workflow
pass, including the planner.

The Chat and Run keep their identities and frozen workflow, model and tool
bindings. Completed graph nodes, selected branches and committed model/tool
context are retained. Continuation survives restarting Aworkit and repeated
Stop/message cycles. Each resumed provider request gets a fresh invocation;
previous tool calls are not replayed. A tool that settles while Stop is pending
keeps its actual result. Remaining undispatched calls in that response receive
explicit non-execution results so the model can reconsider them.

The pipeline writes a `pipeline.stopped-pass` checkpoint before the service
publishes `chat.turn_stopped` with its continuation request and node. The next
accepted input records that source in `command.started`; replay retains the
same choice. Source Chat/Run, frozen context and graph must match. A successful
pass has no stopped checkpoint. Stop records created by older versions without
a checkpoint retain their original fresh-pass behavior.

Verification:

- Rust pipeline tests in `runtime/pipeline/tests/steering.rs` cover both model
  nodes, repeated stops, restart, frozen-authority checks, command replay and
  normal follow-ups.
- Build the frontend and native debug executable, then run
  `node scripts/native-stop-steering.mjs` from `desktop`. It uses an isolated QA
  profile, a local fixture provider and the real WebView composer and Stop
  button. It checks preserved tool results and no replay, including Stop during
  a tool batch. Evidence is written beneath `desktop/src-tauri/target/native-steering-*`.
