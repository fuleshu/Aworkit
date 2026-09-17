# Incremental tool continuation

The large implementation Chat showed 34–38 seconds between a completed shell or
Python call and the next model span. Existing phase timings attributed 23–26
seconds to context preparation. About 1.2 GiB of historical event payloads was
repeatedly decoded and copied after cache invalidation on every append.

## Required behavior

- Preserve durable-before-dispatch ordering, immutable original evidence, frozen
  authority, context ownership, edits, compaction and no-replay recovery.
- Retain immutable shared history snapshots. Successful appends extend the live
  cache; old readers share existing payloads. Reconcile other writers from the
  last known committed sequence rather than restarting at sequence zero.
- Query operational records by kind and identity. A scalar stream head must
  never require loading its payloads. Context restoration must not copy history.
- Keep provider-visible prompt prefixes stable across ordinary tool exchanges.
  Local caching and provider prompt caching are separate mechanisms; cache usage
  must reflect provider evidence, not estimated or invented savings.
- Measure tool terminal to actual provider HTTP request, including settlement,
  context preparation and dispatch. Target less than 1000 ms for every measured
  warm continuation in the release large-history regression. Report cold recovery,
  explicit compaction and external-provider latency separately.

## Verification

Test immutable snapshots held across commits, external append reconciliation,
failed commits, restart, edits, compaction, instruction visibility, and owner
isolation. Use a gigabyte-scale historical fixture and shell/Python exchanges
through the real desktop pipeline; a fresh-Chat microbenchmark is insufficient.
Never run historical tools or modify the user's active history for a benchmark.

## Results (2026-09-17)

The optimized native regression used 14,178 historical events, 1,388,951,040 bytes
of historical snapshots, 101,087,658 bytes of operational records, and an active
context of 2,170,236 bytes containing 450 previous tool exchanges. Four real shell
and four real Python calls took **220, 249, 220, 223, 215, 210, 220, 220 ms** from
their durable terminal timestamps to complete HTTP request arrival at the local
provider. Every request preserved the previous messages and tool definitions
exactly. Report: `desktop/src-tauri/target/native-continuation-1789670773306/result.json`.

The fixture restarts the native application before the measured run. Cold
history hydration happens before its first model call. Normal continuation
retains the decoded payloads; subsequent user turns and queries reuse the same
per-Chat cache. Four recent idle Chats are retained, and active workers are never
evicted. Independent Chats have independent cache locks.

The provider in the affected user Chat is DeepSeek. Its context cache is enabled
automatically and matches persisted prompt prefixes. Stable prefixes are verified
here; a local provider fixture cannot measure actual DeepSeek cache hits or
network/model latency. See https://api-docs.deepseek.com/guides/kv_cache/.

Run the native regression from `desktop` with an optimized QA executable:

```powershell
$qaExecutable = Join-Path $PWD 'src-tauri/target/performance/aworkit-qa.exe'
New-Item -ItemType Directory -Force (Split-Path $qaExecutable) | Out-Null
cargo rustc --manifest-path src-tauri/Cargo.toml --config 'profile.release.package.aworkit-desktop.debug-assertions=true' --release --bin aworkit-desktop -- -o $qaExecutable
$env:AWORKIT_QA_BINARY = $qaExecutable
node scripts/native-continuation-performance.mjs
```

The package debug-assertions override enables the existing isolated-profile test
hooks while retaining release optimization. Production builds omit this override.
