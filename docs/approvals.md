# Tool approvals

Choose an approval mode below the Chat conversation. Settings → Approvals sets
the default for new chats and lists revocable project approvals. Existing
profiles default to **Ask for approval**; an explicit Chat choice persists across
restarts and does not change other chats.

| Mode | Behavior |
| --- | --- |
| Ask for approval | Pause for actions requiring review, unless a matching project approval exists. |
| Approve for me | Send the proposed action and visible conversation/tool evidence to an independent, tool-free request using the Chat's frozen model. Approve, deny with a rationale, or ask the user if the decision is unclear or review fails. |
| Full access | Execute enabled tools without an approval prompt or model review. |

The automatic-review distinction follows [OpenAI's documented reviewer flow](https://learn.chatgpt.com/docs/sandboxing/auto-review).
Aworkit owns its reviewer policy and uses the configured model; it does not call
Codex's hosted reviewer or reproduce Codex's operating-system sandbox. All modes
retain Aworkit's tool-specific execution contracts, disabled-tool checks,
workspace identity checks, cancellation and per-request timeouts. Explicit
workflow Approval nodes remain human workflow decisions.

An approval card offers **Approve once**, **Always approve in project**, and
**Deny and give reason**. A denial is returned to the acting model with the user's
reason and an instruction not to retry or bypass it. Projectless calls and
explicit workflow approval steps cannot create project tool permissions.

Project grants bind the saved project and native workspace identity, exact tool
binding/configuration, and action scope. File edit/write grants cover that
particular file tool within the project's enforced file boundary. Shell, Python,
MCP and other tools retain the exact arguments; a different command, script,
server binding or arguments asks again. Matching uses canonical JSON, including
nested objects. The card displays the scope before it is saved; Settings shows
the action and supports immediate revocation. Grants apply across chats in the
same project and survive restarts.

Read/search/list/grep, task-list and existing web tools keep their ordinary
approval-free contracts. File writes, shell, Python, subagents and MCP tools use
the shared policy. Legacy `requiresApproval` configuration is retained only for
compatibility with existing frozen records and hidden from the tool editor; it
is not the user-facing policy switch.

Automatic-review rationale and provider-reported tokens are recorded as
`approval.reviewed` evidence, separate from the acting model's output and
reasoning. Review cannot execute tools or create standing grants. Invalid output,
missing context, provider failure or timeout falls back to the manual card;
explicit denials return to the acting model. Stop also cancels review.

User choices, grants, automatic decisions and approval results are durable.
The broker still authorizes an exact invocation with a one-use nonce, so a
standing grant does not bypass capability admission. An interrupted approval
command can be resumed with its original identity; committed results are
recovered without repeating a settled provider or tool effect. Human approval
challenges last 24 hours; dispatch revalidates the frozen workspace and tool
constraints.

## Verification

- Native Rust coverage: mode isolation, canonical exact-action matching,
  manual/automatic/full-access execution, review failure fallback, denial reason
  propagation, project grant persistence/isolation/revocation, MCP admission,
  conflicting decisions, and recovery after a lost approval receipt.
- Frontend coverage: decision payloads, required denial reason, projectless
  behavior, Settings save, and existing Chat/Settings regression suites.
- `desktop/scripts/native-approval-smoke.mjs`: the built Windows WebView, isolated
  profile, local streaming provider fixture, real file effects, all three modes,
  all three decisions, cross-chat grant reuse and Settings revocation. It writes
  screenshots and a result record under `desktop/src-tauri/target/native-approvals-*`.

Run the smoke from `desktop` with `node scripts/native-approval-smoke.mjs` after
building `desktop/dist` and the native executable. The fixture uses no remote
provider, credentials, or user profile.
