# Workspace Instructions

Enable **Workspace Instructions** in Settings → Tools, save the configuration, then select it in the Agent node's tool list. Validate and save the workflow. Start a new Chat to use changed Settings or workflow bindings; existing Chats retain their frozen configuration.

The plugin automatically supplies applicable Markdown instructions before model requests. It has no callable model function and works with text-only models. It is independent of Skills. A standalone Tool node cannot invoke it.

## Files and timing

The global file is `~/.aworkit/AGENTS.md`. A custom Aworkit home is configurable. Project discovery starts at the nearest configured root marker within the approved workspace, falling back to the session working directory. The default marker is `.git`, including worktree marker files.

From project root to working directory, each directory loads `AGENTS.md`, `CLAUDE.md`, `AGENTS.local.md`, then `CLAUDE.local.md`. Candidate order is configurable; an empty overlay list disables overlays. Identical paths and same-directory contents equal after trimming are deduplicated. The original content of the first candidate survives. These are ordinary Markdown files; no frontmatter is required. There is no `.dsh` or `DSH_HOME` discovery.

Successful, committed native file reads, writes and edits discover descendant instructions and refresh previously observed scopes, including global instructions. Changes produce addition, replacement or removal messages using the Harness wording. Removed files can reappear. Guidance discovered after a file operation applies to the next model request. Shell commands, search results and arbitrary tool prose are not file-touch signals. There is no watcher.

Each source has a 1 MiB default streaming limit. Each rendered batch has a 64 KiB default UTF-8 limit, including framing. Broader files are omitted first; the most-specific remaining file may be truncated. Omitted and truncated content is identified. Run details → Entire run → Workspace instructions shows file transitions and diagnostics.

## Persistence and context replacement

The existing history stores the bounded rendered message and typed ownership/change metadata. Later requests reference that message at its original conversation position. Retries reuse the admitted preparation exactly. New inputs and resumed requests reconcile current files. State is isolated by Chat, Agent node and branch/child execution context; child activation follows the originating Agent's selection.

The library boundary is `workspace_instructions::prepare(Preparation)`. It receives frozen configuration, authorized filesystem/cwd, owner, durable events, the actual selected event IDs and context revision, settled touches and cancellation. It returns a bounded event, retained references and diagnostics. The desktop's `prepare_automatic_context` performs this reconciliation after context selection for both text and tool loops, then admits the result through existing history before dispatch. Per-owner admission is serialized and replay is idempotent.

Missing baseline and previously active nested instructions are restored before the next request, even without a file touch. Authentic retained events are not duplicated. A summary mentioning instructions does not establish their visibility. Restoration checks current files, respects confirmed removals, and can recover the last admitted bounded text after temporary read failures, with an explicit stale-source diagnostic. Cancellation does not mutate admitted state.

**Loader compaction recovery is implemented and tested; automatic conversation compaction is not yet implemented.** The current desktop context editor provides the real replacement-selection path used by the native test. A future compactor must pass its selected authentic event references and stable replacement revision through this same preparation boundary, preserve durable history, and run the Section 5 integration gate in [the specification](design-workspace-instructions.md). It must not discard loader records or treat summary prose as a retained instruction event.

## Authority and platform boundaries

All reads use the approved filesystem abstraction. The current desktop adapter uses Aworkit's native `ProjectFiles` authority for the frozen project and configured global home. That authority rejects file symlinks; the loader does not widen it. Missing/non-file targets count as absence. Permission, source-size and provider failures count as temporary unavailability. Without an authorized provider, preparation records a diagnostic and remains inert; it never falls back to ambient host reads.

The provider interface supports files with no size/version metadata and is exercised with a provider fixture. This change adds no remote filesystem backend. The current desktop's authorized session/child working directory is its frozen workspace root; the library also supports a deeper authorized cwd. The Unix symlink test is platform-gated and is not claimed as executed by the Windows verification.

## Acceptance evidence

The reference is DeepSeek Harness commit `b150a551b8d465e31e418e1b2eaf5e79bbb7d28e`. The port includes 23 exact render/budget goldens produced from its `render.ts`. Aworkit-specific activation, paths and authority follow the specification.

| Requirement | Evidence |
|---|---|
| WI-01 initial chain | Library tests for nearest marker, worktree marker file, ancestor chain and cwd fallback; native global/project request assertions. |
| WI-02 Aworkit paths | Home expansion/configuration and projectless tests; native custom global home. |
| WI-03 candidates/overlays | Library baseline ordering and empty-overlay configuration tests. |
| WI-04 deduplication | Trimmed duplicate and duplicate-to-removal transition tests. |
| WI-05 prompts/precedence | 23 reference goldens, delimiter escaping and provider-order tests for OpenAI, Anthropic and Gemini. |
| WI-06 nested discovery | Relative/absolute touches and boundary tests; production committed file exchange test; native nested request assertions. |
| WI-07 updates/removals | Library additions, replacement, deletion, duplicate retirement and reappearance; native root changes and nested disappearance/reappearance. |
| WI-08 timing | Production test rejects uncommitted exchanges; provider adapters preserve call/result adjacency; native next-request and historical-position assertions. |
| WI-09 composites | Production authority test retains successful descendant file effects after a failed parent, only after the parent's exchange commits. |
| WI-10 limits | Native source cap/non-file tests, metadata-free provider tests, reference budget goldens and UTF-8 budget sweep. |
| WI-11 partial state | Notice-only/empty-file tests and restoration under budget pressure, retaining omitted scope discovery. |
| WI-12 filesystem | Approved native adapter tests for traversal, nonfiles, missing and oversized sources; unavailable sibling/provider cases. Unix-only symlink test retained. |
| WI-13 cancellation/cache | Cancelled preparation leaves history unchanged; cold same-size rewrite detection; production parallel-owner and one-admission tests. No text cache or background watcher exists. |
| WI-14 resume/reload | Production replay/reopen/cross-input tests, incompatible empty-baseline test; native later Chat inputs and restart. |
| WI-15 restoration | Missing/partial baseline, active sibling scopes, changed/deleted/stale sources, tombstones, repeated replacement, budgets and cancellation; native first provider request after context edit plus restart. |
| WI-16 isolation | Library Chat/node/child ownership tests, production parallel isolation, and native child request assertions. |
| WI-17 inspection | Settings labels/tooltips and Run details projection; native Settings enablement and editor validation screenshots. |
| WI-18 no provider | Missing-provider preparation is inert with diagnostics; no host fallback. |

Reusable tests live in:

- `crates/aworkit-capability-host/src/workspace_instructions/tests.rs`
- `crates/aworkit-capability-host/src/provider_tools/context_tests.rs`
- `desktop/src-tauri/src/runtime/workspace_instructions/tests.rs`
- `desktop/src/workbench/workflowExecution.test.ts`
- `desktop/scripts/native-instructions-smoke.mjs`

Run the native fixture from `desktop` after `npm run build` and `cargo build --manifest-path src-tauri/Cargo.toml --bin aworkit-desktop`. It uses an isolated profile and loopback provider, writes provider requests and screenshots below `desktop/src-tauri/target/native-instructions-*`, and closes its own process. It does not alter the user's Settings or projects.

Windows verification on 2026-09-08 passed the rebuilt native fixture with 12 provider requests across tools, later Chat inputs, restoration, text-only Agent, instruction-only child and disabled scenarios. Evidence is in `desktop/src-tauri/target/native-instructions-1788860183334/`: `requests.json`, `settings.png`, `workflow-validation.png`, and `completed.png`. The provider fixture uses OpenAI-compatible transport; Anthropic and Gemini message ordering/schema omission are covered by adapter tests.
