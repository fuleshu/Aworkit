# Skills and memory

Aworkit's native `skill` tool follows the filesystem skill loader and model prompting in DeepSeek Harness checkout `b150a551b8d465e31e418e1b2eaf5e79bbb7d28e` (`C:\src\deepseek-harness`). Aworkit uses its own directories; it does not discover `.dsh` folders or read `DSH_*` environment variables.

## Using skills

Enable **Skills** under Settings → Tools and select it in an Agent workflow. New Standard Agent templates include the binding. Existing saved workflows and frozen Chats retain their settings; add Skills to an existing workflow and start a new Chat to use it there.

Use the portable `SKILL.md` format rather than an Aworkit-specific syntax:

```markdown
---
name: review-api
description: Review HTTP API changes and their compatibility.
---
Read the changed API handlers and their callers. Check validation,
error responses, and compatibility before proposing changes.
See references/checklist.md when relevant.
```

Create either `<root>/review-api/SKILL.md` or `<root>/review-api.md`. Discovery is one level deep. The required `name` is lowercase ASCII kebab-case; `description` must be a nonempty YAML string. Unknown frontmatter metadata is permitted. Optional `whenToUse` and `metadata` do not affect routing or appear in the model catalog, matching the reference tool.

Search precedence, highest first:

1. `<project>/.aworkit/skills`
2. `<project>/.agents/skills`
3. Additional absolute skill folders in Settings, in configured order
4. `~/.aworkit/skills`
5. `~/.agents/skills`
6. An optional configured bundled skill folder

The project is the nearest ancestor with a `.git` file or directory, otherwise the workspace directory. The global Aworkit home skips its `.system` child. Same-name lower-priority skills are ignored. Home locations and search settings resolve when the Chat configuration freezes. Disabling default discovery leaves only explicitly configured additional and bundled folders.

`disable-model-invocation: true` hides a skill from the catalog and rejects model tool loads. `user-invocable: false` disables direct user gestures. Both default to allowing invocation. Boolean controls accept booleans, `true/false`, `yes/no`, `on/off`, and `1/0`, case-insensitively. Invalid controls and unsupported camel-case spellings reject the whole skill.

A whitespace-bounded `/review-api` anywhere in direct user input loads a user-invocable skill once for that input. Unknown or disabled gestures stay ordinary prose. Tool results, assistant text, and delegated tasks cannot forge this direct user gesture. A skill with model invocation disabled can still be loaded by its explicit user gesture.

## Runtime and prompting

The provider-facing tool is named `skill`, with the exact harness description and one required string argument, `name`. Catalogs contain only sorted names and whitespace-normalized descriptions, capped at 500 UTF-16 units by default. They use the harness's literal initial/replacement `<system-reminder>` wording. Full instructions use the same `<skill_content>`, `<skill_resources>`, and `<skill_instructions>` wrapper and relative-resource guidance. The structured native result is `{name, provider, resourceBase, content}` with provider `filesystem`.

Each model step scans the configured roots. A changed catalog appends a complete replacement at that step's position; an empty replacement explicitly retires earlier names. Malformed/non-text files warn and skip. Unexpected discovery I/O errors retain the last complete catalog. Body-only edits do not republish summaries; a later fresh tool call rereads the body. Referenced resources are loaded only when needed by other authorized tools; the skill tool never enumerates or executes them.

Catalog entries, direct invocation content, and exact exchange positions are committed through the local history store before provider dispatch. Replay reuses committed context and settled tool results. Approval continuation keeps the same history; a later model step can observe current files. A new Agent invocation reestablishes its catalog because Aworkit's current conversation projection carries user/assistant text rather than earlier tool contexts.

The implementation uses fresh per-step scans instead of the harness's Chokidar cache invalidation. This preserves the next-step observation behavior without a second watcher lifecycle. Aworkit's existing authority, cancellation, provider context, durable-record, and tool-output limits still apply. The port implements the filesystem provider; it does not add the harness's Cordis runtime/provider registration APIs.

## Memory investigation

The supplied harness checkout has **no dedicated built-in tool for writing project or global memory entries**, no special memory-entry database, and no built-in memory-writing prompt to copy. This was checked in the package tree, tool definitions/catalog, context plugins, and bundle composition. Files named `tests/memory.ts` implement test doubles, not agent memory tools.

| Mechanism | DeepSeek Harness | Aworkit |
|---|---|---|
| Explicit long-term memory API | No built-in tool found | No built-in tool found |
| Write notes in project files | Ordinary `write`/`edit` filesystem tools | Existing project write/edit tools |
| Write global notes | General filesystem/shell capabilities, subject to configured authority | Existing approved host shell/Python tools; project file tools remain project-scoped |
| Automatically read workspace instructions | `agent-instructions`: user-global and nested `AGENTS.md`/compatible files with durable updates | No equivalent automatic workspace-instruction loader found in the current desktop runtime |
| Conversation persistence | Session persistence with JSONL/SQLite backends | Durable Chat/Run history and optional portable history |
| External memory service | Optional configured MCP server | Existing MCP tool integration, including an optional Adashi server |

The local Harness web profile at `C:\Users\timo\.dsh\profiles\web\cordis.patch.yml` does configure `mcp-adashi`, using `C:\Program Files\Adashi\adashi-mcp.exe` over stdio. The available Adashi API provides `adashi_get_memory`, `adashi_append_memory_note`, coordinator-only `adashi_update_memory`, and `adashi_update_memory_rule`. These all require a project identifier; there is no separate global-memory scope in that API. Its protocol says to read memory before each task and append a concise immutable, idempotent handoff note after a successful task, keyed by run and optional task; ordinary agents must not replace or compact canonical memory. This behavior belongs to the external Adashi service and its project protocol. Aworkit's existing MCP configuration, discovery, frozen bindings, and dispatch can expose the same server and its unchanged tool prompts. No native replacement or profile configuration change was made.

Session persistence records conversations; it is not an agent-callable memory-entry API. The repository's Agent Notes workflow is also instructions for ordinary file operations, not a special memory tool. No new memory tool or invented memory prompt was added because the requested built-in reference feature is absent. Automatic workspace-instruction loading is a separate feature from the requested memory-writing tools.

Reference sources: `packages/skill/{skill,skill-filesystem,tool-skill}/src/index.ts`, their READMEs, `packages/context/agent-instructions`, `packages/session/session-persistence`, `packages/storage/storage-domain`, and `docs/tool-catalog.md` in the supplied harness checkout.

## Verification

The workflow editor validates built-in tool bindings from the shared bundled manifest. A native registry test checks that the manifest and Rust executor admission list agree. This fixes the stale frontend list that rejected `tool.skill` with "no installed executor" even though the executor was installed. Regression coverage includes Agent and Tool node bindings, unknown-tool rejection, editor Validate/Save/Run, and native Skills checkbox selection before the smoke test starts a Chat.

Skill tests cover discovery precedence, rejected DeepSeek folders, frontmatter controls, resource bases, body changes, renames, cancellation, and exact prompt markup. Native authority tests cover durable catalog replay, replacement/retirement, direct invocation, settled-result reuse, and rejected model loads. Provider tests check the same context positions on OpenAI-compatible, Anthropic, and Gemini wires. `desktop/scripts/native-skills-smoke.mjs` exercises an isolated native profile and a local provider fixture; it never uses a hosted model or modifies the user's profile.

Verified on Windows: 232 desktop Rust tests; 68 capability-host library tests (two unrelated live diagnostics ignored), plus the added non-file/non-text discovery regression; 12 provider integration tests; 58 focused frontend Settings/workflow tests; TypeScript/Vite production build; native debug build; and the actual WebView smoke with five local provider requests. The additional provider-context regression also verifies literal skill error text on OpenAI and Anthropic. Strict Clippy is blocked by existing warnings in `aworkit-process`; `--no-deps` completes with existing web-provider warnings. No hosted model was used.
