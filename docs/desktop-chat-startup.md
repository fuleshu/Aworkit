# Loading long Chats

`desktop_chat_snapshot` returns authoritative Chat/sidebar metadata and the
newest contiguous event window. `desktop_chat_events` retrieves earlier windows
on upward scrolling, or a missing live tail. Each read is pinned to a Chat and
committed head. The client rejects gaps, foreign streams, conflicts and events
beyond that head. An omitted prefix is explicitly represented by `firstSequence`;
it is never mistaken for a fully replayed history.

The page budget (4 MiB JSON, 128 events) controls transport batching,
not Chat length. One larger event is returned intact. No stored inputs,
checkpoints, events, or evidence are removed or replaced. This prevents a long
Chat from being serialized into one enormous native/browser IPC response.

Span lifecycle/content records crossing a page boundary are fetched as exact
supporting facts, separately from the contiguous window. Ancestors supply
context without pulling in their unrelated children. The inspector labels
partial history as loaded activity. The complete canonical records remain in
SQLite; no history-length or message-length ceiling is introduced.

The native event loop creates the window and starts storage, migrations and
recovery on a blocking worker. Page reads release the coordinator and use
independent SQLite reader connections. Metadata queries exclude unrelated
large inputs/checkpoints. Unchanged polls preserve the renderer's event array.

Chat selection immediately shows the selected title and a local busy indicator.
Only navigation writes are ordered; page loads run independently. Generation
fences prevent obsolete reads from replacing a later selection. Loading earlier
activity has its own indicator and retry control, and preserves the visible
row. Programmatic layout changes do not request more history. Failed Chat loads
do not disable navigation to another Chat.

From `desktop`, run `node scripts/native-startup-smoke.mjs` after building the
frontend and a standalone debug host (`TAURI_CONFIG` with `build.devUrl: null`).
The script defaults to an isolated profile. Explicitly setting
`AWORKIT_STARTUP_PROFILE` tests a supplied profile without sending messages or
editing Chat content. It opens and closes that profile normally, verifies the
composer and bounded response, measures frame responsiveness during loading and polling,
scrolls upward and checks anchoring, holds one Chat response while another loads,
and restores the original selection. It saves a screenshot and JSON result under
`src-tauri/target/native-startup-*`.
