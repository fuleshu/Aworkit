<!-- Generated Adashi skill 'memory'; do not edit this copy. -->
# Skill: Project memory

Read this when the run needs a prior decision, constraint or blocker, or when you are about to
write a handover note.

## Reading

- The run.start injection carries the current project summary within its own budget. Treat it as
  the project's current constraints, not as history: superseded handovers are excluded, and
  retrieved historical notes are dated evidence rather than current state.
- When the summary does not cover something the run needs, retrieve it with `adashi_memory` `get`
  using `query`, `runId` or `taskId`.
- If the summary was omitted for size and the work needs project constraints, retrieve it before
  proceeding. Operational requests can select `memoryContext: "protocolOnly"` to omit summary
  context.
- Include superseded notes only when their provenance matters.

## Writing

- Writing a note is optional. Keep only important decisions, non-obvious constraints, or
  unresolved blockers with a concrete next step.
- Skip routine reports, successful checks, and facts already held by design, tasks or QA.
- Keep each handover within 1,000 characters and identify its run and optional task.
- Only an authorized coordinator may replace the shared summary or supersede reviewed notes.
- Retention is bounded: at most 20 notes and 12,000 total characters, oldest first.

Fetch `adashi_help` for `adashi_memory` to see the append, update and rule parameters.
