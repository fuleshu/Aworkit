# Workflow node catalog

Every node type this build executes, with the configuration keys the executable
catalog accepts. "Required" keys must be present; the rest may be omitted. A
type with neither column filled must have an empty (or absent) `configuration`.

The table below is the authority the drift check compares against the code -
when a node type or key is added or removed, this file must change with it.

| node type | required configuration | optional configuration |
| --- | --- | --- |
| `input` | — | — |
| `loop` | `exitCondition` | `maximumIterations` |
| `agent` | `modelTierId`, `toolIds` | `instructions`, `reasoningEffort`, `enableThinking`, `compaction`, `timeoutSeconds` |
| `model_call` | `modelTierId` | `instructions`, `maximumTokens`, `outputContract`, `reasoningEffort`, `enableThinking` |
| `tool` | `toolId` | `parameters` |
| `external_agent` | `toolId` | `instructions`, `model`, `reasoningEffort` |
| `condition` | `predicate` | — |
| `parallel` | — | — |
| `approval` | — | `title`, `message` |
| `output` | — | — |
| `wait` | — | — |
| `completion` | — | — |

## Key details

| key | accepted value |
| --- | --- |
| `modelTierId` (`agent`, `model_call`) | `tier:<name>`, a model tier configured in Settings |
| `toolIds` (`agent`) | array of unique binding ids: `tool.<name>`, `comfyui.<tool id>` or `mcp:<server>` |
| `toolId` (`tool`, `external_agent`) | one binding id; on `external_agent` it must be an installed delegation tool, and on `tool` it may not be an automatic-context plugin (those belong on Agent nodes) |
| `instructions` | non-empty text, at most 64 KiB |
| `reasoningEffort` | `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, or null to inherit |
| `enableThinking` | boolean, or null to inherit |
| `maximumTokens` (`model_call`) | integer 1..=8192 |
| `outputContract` (`model_call`) | `plan` when present |
| `parameters` (`tool`) | JSON object passed to the tool |
| `timeoutSeconds` (`agent`) | legacy: integer 30..=3600 when present; the value is ignored |
| `title`, `message` (`approval`) | text, at most 4 KiB and 16 KiB |
| `exitCondition` (`loop`) | predicate object (see below) |
| `maximumIterations` (`loop`) | positive integer; omit it for a loop that repeats until its exit condition holds |
| `compaction` (`agent`) | object with `auto` (boolean), `pruneToolResults` (boolean), `thresholdChars` (1..=4 MiB), `headChars`/`tailChars` (0..=1 MiB) |
| `predicate` (`condition`) | predicate object (see below) |

## Predicates

A predicate is `{"kind": ...}` where `kind` is one of:

| kind | fields |
| --- | --- |
| `always` | — |
| `exists` | `path` |
| `eq`, `neq` | `path`, `value` |
| `and`, `or` | `operands`: 1..=8 predicates |
| `not` | `operand`: one predicate |

Predicates may nest at most 4 levels.

`eq` and `neq` compare the value exactly, with one exception: two strings compare
with surrounding whitespace ignored. A `model_call` answer is trimmed before it
reaches the graph, and a condition still matches when a provider pads its answer
(`"\n\nSIMPLE"` equals `"SIMPLE"`). Text never equals a number, a structured
value or null.

## Routes

| route | used by |
| --- | --- |
| `true`, `false` | `condition` node's two outgoing transitions |
| `body`, `exit`, `fallback` | the three routes out of a `loop` header |
| `feedback` | the one edge that closes a loop's region |

## Bundled templates

`Simple`, `Standard`, `Planer` are seeded on a fresh profile;
`Blank`, `Triage Router`, `Evidence Brief`, `Iterative Planning` and
`Delegated Code Review` ship as templates.
