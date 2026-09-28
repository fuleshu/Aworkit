<!-- Generated Adashi skill 'qa-jobs'; do not edit this copy. -->
# Skill: QA jobs

Read this before creating, changing or running a QA job. A QA job is one check of one behavior,
not a pipeline; the server enforces the contract below and rejects violations.

## Job shape

- Every job declares `kind` (`lint`, `unit`, `integration`, `e2e`, `smoke`, `build`, `release`)
  and `scope`, one sentence naming the single behavior it verifies.
- A runnable job must link the design specification or task it verifies. A disabled draft may omit
  the link while you choose one.
- `timeoutSeconds` is capped by kind: lint 120, smoke 300, unit 600, integration 900, e2e/build
  1800, release 3600. Never raise a timeout to make an oversized job fit; split the job.
- Only `kind=release` may package or bundle. A test job that runs `tauri build`, an installer or a
  bundle assembler is rejected: building a deliverable is not verifying behavior.

## Running

- Name the selection: at least one of `jobIds`, `states`, `tags`, `taskIds` or
  `designExternalIds`. An empty query over every enabled job needs `allowBroadRun=true`.
- No run may exceed 12 jobs. `maxDurationSeconds` (default 900, maximum 3600) bounds the whole run;
  a job may not outlive the remaining budget, and jobs that never start are recorded `skipped`.
- Jobs whose latest evidence is already green are skipped unless `force=true`.
- One live run per job. A job whose worker dies is reclaimed as `interrupted` once its lease
  expires; a live run can be stopped with `cancel_run`.

## Shaping a job well

Prefer many small jobs selected together over one shell script that chains them. When a change
needs several checks, run the smallest set that covers it. Do not re-run a milestone aggregate to
verify one behavior; select the narrow job that owns that behavior. Read run output with `get_run`;
listings never inline console output.

Fetch `adashi_help` for `adashi_qa` to see the exact create, update and run parameters.
