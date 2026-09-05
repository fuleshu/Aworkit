# Desktop notifications and Settings return

Approved design and implementation, 2026-09-05. Scope: presentation across the desktop application.

## Observed problem

Before this change, `desktop/src/styles.css` positioned Settings and workflow command banners absolutely at `top: 48px` over the body. `SettingsScreen.tsx` retained its success banner until another operation or edit replaced it. The independent `App.tsx` notification used a fixed overlay and manual dismissal only. Chat errors also had a separate modal queue. These paths had no shared lifecycle.

`App.tsx` already kept visited routes mounted and hid inactive surfaces. This preserved drafts and view state, but navigation only changed a route and focused the main surface. Settings had no close callback or captured return context.

## Product behavior

### One permanent bottom status bar

- Reserve a full-window row below navigation, workspace, and inspector. Default height is 32 logical px, scaled with application text size. Notifications never sit over workspace content. Empty and occupied states use the same height.
- The shell grid has a flexible content row and the reserved status row. Preserve `min-height: 0`, `min-width: 0`, and overflow containment through intermediate panes. Splitters stop above the status row. Do not implement this with an overlay plus guessed bottom padding.
- Show severity icon, concise message, optional action, notification-list button with active count, and Dismiss. Quiet state reads `Ready`; this means no active notice and does not claim all services are connected.
- Display one primary message. Priority: action required, error, warning, progress, then success/information. Within a priority prefer the current context; otherwise keep unresolved actions in creation order and show the newest informational update. Do not rotate messages on a timer.
- Additional active messages remain reachable through the list button. Opening it explicitly adds a docked, scrollable details row above the bar and reduces workspace height; it never covers content. Collapse it on route change. Show full message text and source context here, with separate Active and Recent sections. Recent is collapsed by default.
- Long summaries truncate in the single-line bar; full text and actions remain keyboard-accessible in details. At narrow widths move secondary actions into details, retaining severity, message, list, and dismissal. Keep the bar anchored and readable at 200% zoom and across light, dark, and forced-color modes.

### Relevance and lifetime

Severity describes meaning. Lifetime is a separate explicit field; an error is not automatically permanent.

| Kind | Default behavior | Becomes irrelevant when |
| --- | --- | --- |
| Success, such as Settings saved | Remove after 5 seconds | Timer expires; a newer edit/save supersedes the message; originating Settings visit closes |
| Information | Remove after 8 seconds | Timer expires or its context changes |
| One-off warning | Remove after 12 seconds | Timer expires, source changes, or user dismisses |
| Failed operation | Remove prominence after 15 seconds; retain source error and Recent entry | Retry supersedes that attempt, relevant edit invalidates it, or user dismisses; no false success |
| Operation in progress | Update one record; no arbitrary success/failure timer | Actual completion, cancellation, or authoritative failure replaces it |
| Ongoing condition/action required | Present only while condition holds | Fresh source projection confirms resolution; entity disappears; user leaves a purely local scope |

All one-off messages can be dismissed early. Countdown pauses only while the user hovers or focuses that message/details, and is removed immediately if its source becomes invalid. Otherwise deadlines continue while queued, hidden, or the window is inactive. Reconcile expired deadlines on visibility/resume so old messages do not reappear in a burst. An accessibility preference can extend transient durations or make one-off dismissal manual; source invalidation always wins.

Dismissal acknowledges presentation, not the underlying condition. A dismissed ongoing condition loses its primary-message slot, remains discoverable as a compact active count and in its owning surface, and disappears from Active as soon as resolved. Identical polling/events must not reopen it. A genuine resolved-to-failing transition creates a new occurrence and may notify again.

Every notice has an explicit owner and scope: application, route visit, entity/version, or operation attempt. Leaving Settings clears that visit's notices even though React keeps Settings mounted. A late response cannot recreate a departed visit's banner. Global disconnection and still-running background work remain relevant across navigation. Changing a provider/project/form invalidates diagnostic results tied to its old fingerprint. Do not equate any navigation with resolving a global problem.

Progress follows the command owner, including its existing deadline and cancellation semantics. On loss of fresh state, replace a spinner with `Status unavailable` and an existing safe Resync/Inspect action; never infer failure or repeat an effectful command. Missing an expected response triggers reconciliation, not endless `Saving…`. Notification dismissal never cancels work.

### What is and is not a notification

All transient success/error/information messages, connection/projection health summaries, and command progress use this shared bar. Migrate Settings banners, workflow/library notices, management command/stale summaries, Chat command and execution error notices, missing-dependency summaries, and native-presentation fallback messages. Informational error dialogs become bar notices with an Inspect action.

Field validation stays beside its field; capability descriptions (such as the screenshot's credential-injection limitation) become neutral help text; read-only document labels, execution history, and detailed probe results remain in their owning content. They are not transient notifications. Genuine consent/confirmation, approval, and interrupted-command decisions retain their explicit decision controls. A bar summary may link to them but dismissal/expiry must never imply a decision or unlock a command.

Foreground application feedback always reaches the bar, including messages that currently invoke a native OS message/notification first. Existing optional background OS notifications can mirror the same committed event through the native adapter, deduplicated by event identity. No new OS notification policy or permissions UI is part of this change.

## Minimal architecture and contract

One shell-owned `NotificationStore`, exposed through a narrow `NotificationPort`, feeds a `DesktopStatusBar` and its docked details view. Feature adapters own relevance and publish facts from existing commands/projections. The store owns only presentation: deduplication, ordering, expiry, acknowledgement, and a bounded session-only Recent list. It performs no provider calls, canonical writes, retries, or authority decisions.

`NotificationV1` contains `id`, `dedupeKey`, `occurrence`, `severity`, `summary`, optional redacted `detail`, `scope`, `sourceVersion`, optional `operationId`, `lifetime`, and optional typed `action`. Lifetime is a closed union: transient with expiry, operation with operation identity, or condition with condition/occurrence identity. No unowned persistent string is accepted.

Port operations are `publish(notice)`, `update(id, occurrence, patch)`, `resolve(id, occurrence)`, `dismiss(id, occurrence)`, `clearScope(scope)`, and a read-only subscription. New attempts receive new occurrence/generation tokens. Older updates and timer callbacks cannot clear or replace a newer occurrence. Feature adapters use explicit relevance inputs; the store never interprets message wording to decide whether it is current.

Route-visit scopes are created on entry and disposed on exit, independent of mount/unmount. Existing generation guards and diagnostic fingerprints in Settings remain authoritative. Initial snapshots/historical execution failures populate canonical views without emitting new notices; only newly observed relevant transitions notify. Keep event cursors/occurrence watermarks so discarded Recent entries cannot cause replayed failures to notify again.

Keep at most 50 redacted Recent summaries, at most 24 hours old, in memory only. Never persist secret inputs, notification bodies, or DOM references to local storage. Do not duplicate the canonical event ledger. Coalesce active notifications to one per live condition or operation; aggregate large sets of background run conditions into counted groups with source links. Expired notices are removed from primary/Active immediately; Recent is optional retrospective context, never a queue to replay.

Use one nearest-expiry timer and separate store subscriptions so progress does not rerender the chat timeline or workflow canvas. Emit live-region text once per meaningful transition, not per progress tick. Routine updates use polite status; an immediate actionable failure may announce assertively once without taking focus. Stable actions preserve keyboard focus. If a focused item resolves, move focus within details or back to its list trigger without moving it to the workspace unexpectedly.

For observability, debug diagnostics may record notification ID, owner, and transition reason (expired/resolved/superseded/dismissed), not message bodies or secret material. UI summaries use existing redaction and plain text. Any notification action delegates to an existing typed intent and rechecks availability against current core state.

## Settings opens as a reversible visit

- Add a visible left-arrow `Back` button before the Settings title. Accessible name/tooltip identifies the destination, such as `Back to Workflows`. Do not label it `Exit`, which could mean quitting the application.
- The shell captures one `SettingsReturnContext` only when entering from a non-Settings surface: route, stable Chat/Run or workflow identity, selected item/tab, inspector state, split sizes, navigation expansion, scroll anchors (including follow-latest mode), and last focused control/caret.
- Retain the existing mounted surface and its draft/undo state; do not rebuild it on Back, start a New Chat, select a default workflow, reset panels, or overwrite a workflow draft. Capture/restore view anchors as necessary where hidden-layout effects would otherwise change them. Hidden surfaces continue receiving runtime facts but must not steal focus, reset selection, or run visible-only autoscroll/layout effects.
- Repeated Settings clicks, native-menu actions, shortcuts, and internal section links do not replace the original return context. All entry points use `openSettings(context)`.
- Back closes the Settings visit and restores the captured view. Restore layout/selection, then scroll anchor, then focus/caret with scroll prevented. A view that was following the chat bottom continues doing so; a manually scrolled view keeps its anchor. Live execution and committed appearance/configuration changes remain current: returning restores the workspace, not an old runtime snapshot.
- Escape invokes Back only when no child popup/dialog/editor consumes it and no IME composition is active. It never closes the application. Normal sidebar navigation to another destination uses the same Settings leave guard, then honors that explicit destination.
- If there are unsaved changes, show exactly `Save and return`, `Discard and return`, and `Stay in Settings`. Saving leaves only after the existing receipt/snapshot postcondition is verified. Validation errors, conflicts, unknown outcomes, and failed saves keep Settings open with all drafts intact. Discard restores the latest canonical data, including appearance preview, before leaving; if reconciliation fails, preserve the draft and remain.
- While an irreversible Settings mutation is pending, do not launch another save/discard. A Back request waits for its existing reconciliation and leaves only after verified success; failure stays in Settings. Read-only probes can cancel/detach on leaving. Clear secret input buffers on actual exit, and reject late visit-scoped callbacks.
- With no dirty draft and no pending mutation, Back is immediate. If the original entity has actually disappeared, return to its containing view with a brief explanation and preserve any local draft for recovery. With no captured origin, return to the existing Chat workspace without sending New Chat.

## Implementation order and verification

1. Add the store/port, shell status row and docked details. Preserve pane containment and wire native fallback messages to it.
2. Migrate each message source above, defining its scope, condition/attempt identity, expiry, and invalidation. Remove obsolete overlay selectors and informational modal rendering. Keep validation, authority, and canonical error contracts intact.
3. Add the Settings visit/leave controller and Back control; route all settings entry/exit intents through it. Pass route activity/visit identity to mounted feature surfaces.
4. Test behavior with a controlled clock: expiry while queued/backgrounded, hover/focus pause, resolution while paused, repeated condition polling, dismissal then recurrence, retry/edit generations, and late callbacks after leaving. Test priority/count/list and source navigation without implicit retries.
5. Test round trips from Chat and workflow editor with non-default selection, unsent text/attachments, scroll, canvas pan/zoom, undo history, inspector and splitter positions. Include repeated Settings entry, Escape/child dialogs, every dirty-draft choice, failed/uncertain save, deleted origin, and live streaming while Settings is open.
6. Build the frontend before the native executable and verify the actual Windows WebView2. Assert every workspace pane/composer ends above the bar, list expansion consumes layout space, and long messages/200% text/dark/light/keyboard/reduced-motion states remain usable. Browser mockup checks demonstrate the proposal only; they do not prove application behavior.

Acceptance: no app notification obscures a real control; the screenshot's save confirmation expires after five seconds or a newer edit/exit; resolved conditions leave Active immediately; no stale replay revives an acknowledged notification; Settings Back restores the previous workspace without losing its drafts or view state.

## Implementation and verification

The shell owns `desktop/src/notifications/NotificationStore.ts` and the docked `DesktopStatusBar`. Feature publishers cover Settings mutations and diagnostics, workflow/library outcomes and compatibility, Chat failures/recovery/connection state, Management, and native presentation messages. The old notification overlays and Chat error modal are removed. Timing preferences are session-only; Recent retains no action callbacks.

`shell/settingsNavigation.ts` captures the return view and closes visit scopes. Settings leave hooks retain drafts until Save or Discard is verified; Save checks both receipt/version and canonical content. `chat/useTimelineReturn.ts` restores a message anchor after hidden updates or text-size changes. Secret input editors are reset on actual exit.

Verification on 2026-09-05:

- 218 frontend tests passed, including controlled notification clocks, stale generations, acknowledgement/recurrence, concurrent reading pauses, late diagnostics, all leave decisions, failed/uncertain/conflicting saves, canonical-content mismatch, draft/caret restoration, workflow Undo, and message anchors. The Chat runtime suite also passed after the final Chat-scoped recovery identity change.
- TypeScript and Vite production build passed; the Windows debug executable was rebuilt from those assets. Vite retains its existing chunk-size/mixed-import warnings.
- Actual Windows WebView2 checks in an isolated debug profile verified a native save confirmation expiring after five seconds with unchanged workspace geometry; Save and return; Stay/Discard; preserved Chat draft/caret/inspector width; workflow selection/viewport/Undo; and the details dock reducing workspace height by 280px. At 760×560 with 200% text, the bar reserved 64px and stayed within the viewport in light and dark modes.
- Native QA uses `npm run test:native-notifications`. Launch the debug executable with `AWORKIT_QA_PROFILE` pointing to an isolated profile and `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9224`; set `AWORKIT_SETTINGS_QA=1` and `AWORKIT_CDP_URL=http://127.0.0.1:9224` for the test. Results/screenshots are written under `desktop/src-tauri/target/notification-qa`. This native scenario changes only its isolated profile's appearance and does not send model requests. Hidden message updates are covered by deterministic tests.
