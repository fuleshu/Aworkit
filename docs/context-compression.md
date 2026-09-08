# Context compression and retrieval

Headroom evaluation and implementation design, 2026-09-08.

Reference pinned to [Headroom e67b3c8](https://github.com/headroomlabs-ai/headroom/tree/e67b3c8a29443a60d6b0018fb22f525c5cd7e709). This complements the existing [DeepSeek compaction](context-compaction.md). Compression reduces individual fresh observations; compaction consolidates an older, balanced conversation prefix. Both preserve canonical evidence.

## Source audit and decisions

| Mechanism and source | Finding | Aworkit decision |
| --- | --- | --- |
| `transforms/live_zone.rs`, proxy `compression/live_zone_*` | Only mutable latest blocks are eligible; raw JSON range surgery preserves cached wire prefixes. The core live-zone dispatcher still has explicit no-op source/prose/HTML branches; standalone engines and Python routing have broader coverage. | Transform a settled observation once, before its first provider admission, then reuse its durable projection. Aworkit owns provider serialization, so stable typed messages preserve prior provider content without proxy surgery. Never rewrite user/system/assistant messages, tool arguments, signatures, or instruction contributions. |
| `smart_crusher/compaction/{compactor,ir,formatter,walker}.rs` | Schema union, sparse cells, nested tables, discriminator buckets, optional opaque offload. Lossless-first wins at 15% byte savings. Stringified JSON and opaque cells require particular care about type and original recovery. | Typed tables with explicit missing cells and exact JSON values, constant factoring and recursive nesting. Validate decoding against the original value before accepting. No implicit conversion of strings into JSON values. |
| `pipeline/reformats/log_template.rs` | Drain-like positional templates retain variable columns and order, reducing repeated log text without deleting observations. | Reversible consecutive templates preserving whitespace, identifiers, dates, addresses, counts, and line endings. Verify byte-for-byte reconstruction. |
| `smart_crusher/{planning,outliers,statistics,anchors}.rs`, `adaptive_sizer.rs` | Row selection combines boundary anchors, structural rarity, numeric deviations/change points, categorical tails, exact query anchors and relevance. Adaptive sizing uses information coverage/knee detection and diversity checks. | Adaptive mode uses protected rows plus query BM25 and marginal information coverage, preserving original indices. Numeric and structural anomalies, rare categorical values, errors, and configured literal protections are mandatory; if they defeat savings, retain the original. |
| `log_compressor.rs`, `search_compressor.rs`, `diff_compressor.rs` | Severity/stack/summary scoring, query relevance, file diversity, context around changes, bounded selections. Some paths cap even errors or drop whole files/hunks. | Preserve every detected error/stack region and every diff change/header. Retrieve omitted context by original offsets. Search/prose extraction retains complete source spans, ranked by relevance and diversity, with explicit omissions. |
| `code_compressor.rs` | Tree-sitter AST selection preserves declarations/signatures/imports; compressed code is reparsed. Eight Rust language grammars; unknown or malformed code falls back. | Parse supported code, retain declarations and query-relevant/error-bearing function bodies. Emit an explicitly labelled outline with original byte ranges; never present an outline as executable source or an edit base. Unknown/invalid syntax is unchanged. |
| `text_crusher/crusher.rs`, `relevance_split.py`, `relevance/bm25.py` | Deterministic sentence extraction, coherent segments, relevance, recency and duplicate suppression. Python split incorporates the current question and triggering arguments. | Shared Unicode-aware lexical retrieval and extraction; preserve complete spans and order. No generated facts. Only adaptive mode may omit prose. |
| `ccr/mod.rs`, backends and `ccr/tool_injection.py` | Content hashes and injected retrieval tools make lossy selection recoverable. Default idle TTL 30 minutes, absolute lifetime eight times TTL; eviction can outlive a visible marker. Raw store interface is put/get, not an Aworkit authority boundary. | Canonical `tool.context` plugin provides bounded read/search/stats through the existing broker. Durable owner-scoped references live with Chat history, without a shorter TTL. Commit originals before emitting references. No background server, proxy, credentials or global cache. |
| `cache/compression_feedback.py` | Retrieval rate is a signal of excessive compression and adjusts later hints. It is not proof of answer quality. | Owner-local feedback backs off adaptive extraction for a repeatedly retrieved capability; previous projections never change. Record measured bytes, estimated/tokenizer counts, latency, retrieval and pass-through reasons separately from model billing. |
| `kompress.rs`, `image/trained_router.py` | ModernBERT word keep/discard inference (ONNX, optional weights) and MiniLM/SigLIP image decisions are lossy. Keeping words is not semantic losslessness. | Do not silently download models, delete individual prose words, downsample vision inputs or lower the user's reasoning settings. Deterministic span extraction plus retrieval is implemented here. A configured external Headroom MCP server can already use Aworkit's frozen MCP path, but it is not implicitly installed or granted automatic transformation authority. |
| `benchmarks/index_proof_table.py` | Seeded synthetic token-count scenarios; published full-corpus results use `protect_recent=0`, different from defaults. No model call in this benchmark. | Measure Aworkit's own savings and latency on reproducible fixtures; independently test retention/reconstruction and native provider payloads. Do not claim universal quality parity or monetary savings from bytes alone. |

## Implementation plan and contract

1. Add a focused, provider-independent Rust compression module: policy, token accounting, reversible formats, statistical selection, text/code extraction, and bounded retrieval. Pure transforms receive data and frozen policy; they own no authority or network.
2. Add per-model compression settings under Context compaction: off/lossless/adaptive, minimum size/savings, target fraction, tokenizer, code/prose extraction, literal protections and excluded capabilities. Missing policy in an already frozen Chat means unchanged behavior. New Chats freeze the default lossless policy. Adaptive selection requires the retrieval plugin in that Agent's frozen definitions; otherwise use lossless only.
3. Extend the canonical native plugin manifest with `tool.context` (`aworkit_context`), operations read/search/stats. Retrieval takes an opaque reference, optional JSON pointer or UTF-8 byte offset, and bounded page size; search returns ranked exact spans. No model-supplied Chat/node/owner selector. Results of retrieval are never compressed again. All arguments and grants pass through the existing broker.
4. Integrate before the first model-facing output cap, after canonical tool settlement. Retain original result and a durable projection keyed by owner, original hash, invocation/call and policy. Commit-before-use; replay the same projection for retries/reopen. Register each outer invocation's owning node/child at admission. Authorization derives from this committed scope, never a marker supplied by the model. Forks explicitly copy only the parent's owner-scoped archive; children have independent retrieval access and feedback.
5. Keep metadata, non-text rich blocks, errors, Skills and Workspace Instructions intact. Selection and pressure measurement see compressed results. DeepSeek's later pruning/summarization remains independently configured. A retrieval reference remains resolvable after a summary. Existing resource limits and cancellation still apply; failure cannot publish an unresolved reference.
6. Validate reconstruction, protected facts, anomalies, identifiers/Unicode, honest omissions, size gates, retrieval paging/search, cache stability, frozen policy, owner isolation, restart/fork and original evidence. Exercise the actual native workflow editor and provider request path with deterministic fixtures.

## Quality boundaries

Lossless means exact source bytes for text templates and exact typed JSON values for tables; object whitespace is not semantic JSON data. Adaptive means omitted content remains retrievable, not that every answer is unchanged. Protected content exceeding a target is retained rather than forcibly clipped. All sizes include the displayed format/retrieval metadata. Compression is local and consumes no auxiliary model tokens. Provider billing and latency still depend on the model, cache, and whether retrieval is needed.

## Configuration and execution

In **Settings → Providers & models**, open a model's **Tool result compression** panel. The settings are frozen when a new Chat starts. New Chats default to lossless mode; existing frozen Chats without a compression policy retain their previous behavior.

For adaptive extraction, enable **Context retrieval** in **Settings → Tools**, then select it in the workflow Agent's tools and save the workflow. This uses the normal native plugin registry, frozen binding, broker validation and child allowlist. Its read-only authority is derived from the admitted invocation; no approval or credentials are needed. Disabling the selected retrieval capability prevents adaptive deletion.

The model record uses this policy (all omitted fields have these defaults):

```json
{
  "compaction": {
    "compression": {
      "mode": "lossless",
      "minimumBytes": 2048,
      "minimumSavings": 0.15,
      "targetRatio": 0.4,
      "tokenizer": "estimate",
      "extractCode": true,
      "extractProse": true,
      "protectedText": [],
      "excludedTools": []
    }
  }
}
```

Modes are `off`, `lossless`, and `adaptive`. Token counters are `estimate`, `cl100k`, and `o200k`; choose an embedded tokenizer only when it matches the provider's vocabulary. The estimator uses the existing UTF-16 context convention. No tokenizer model is fetched at runtime.

The pipeline runs on a **newly settled tool result**, before the existing model output cap and before the next context-pressure check. It does not run every time old history is serialized. It admits a representation only when both counted tokens and bytes improve by the minimum savings, including format instructions and reference metadata. Adaptive selection competes with the lossless representation; an extraction failing the final token gate falls back to lossless. Inputs larger than 512 KiB pass through to the existing tool limits. Other work bounds include 8,192 rows/64 columns per reversible table, 2,048 adaptive rows or spans, nesting depth 16, and a 100 ms syntax parse limit. An unsuccessful transformation leaves the result unchanged.

Syntax outlines cover Rust, Python, JavaScript/JSX, TypeScript/TSX, Go, Java, C and C++. Unsupported source paths and failed parses are not treated as prose. Rich result envelopes retain image/resource/reasoning blocks, annotations and ordering. Source span offsets are UTF-8 byte positions. Reversible formats carry their decoding instructions; sparse tables distinguish absent keys from explicit null.

`tool.context` has provider name `aworkit_context` and a configurable `maximumBytes` of 256–65,536 (default 16,384), further bounded by the Agent output limit:

```json
{"operation":"read","reference":"<64 hexadecimal characters>","pointer":"/content","offset":0,"limit":4096}
{"operation":"search","reference":"<64 hexadecimal characters>","pointer":"/content","query":"identifier_739"}
{"operation":"search","query":"identifier_739","offset":0}
{"operation":"stats"}
```

Reads paginate original bytes; referenced searches paginate relevance-ranked source spans. Unreferenced searches scan at most 64 authorized archives per page and return references, allowing evidence discovery even after a summary forgets a marker. `nextOffset` identifies the next page. Search previews favor rare query matches within oversized records; exact positions allow a subsequent read around a hit. Every original is integrity-checked before access, and every serialized response must fit the configured bound.

Originals and projections are retained in canonical Chat semantic history, alongside the original broker outcome. This intentionally costs additional local storage; it avoids a shorter-lived external cache or references that stop working after a restart. History retention/deletion owns their lifetime. A failed archive commit stops admission; recovery reopens the broker's settled original without rerunning the tool. Forks copy the selected parent node's archive with provenance and new Chat ownership. Child archives, retrieval queries and feedback are separate from the parent.

After at least three compressed observations of a capability, retrieval of at least one third of its distinct references changes future observations in that owner scope to lossless mode. Existing projections remain unchanged. The context usage panel reports cumulative local bytes saved and retrieval calls; these are not billed tokens or net savings after subsequent retrieval.

## Verification and measured results

Validated on Windows on 2026-09-08:

- Capability-host suite: **106 passed**, two existing ignored tests. Coverage includes exact JSON/text reconstruction, sparse/nested/null/hostile-key cases, anomalies, protected evidence, eight syntax grammars, Unicode pagination, long-record retrieval, rich block preservation and whole-envelope savings gates.
- Desktop suite: **281 passed**. New broker tests cover durable projection/reopen, scope isolation, archive discovery, legacy and disabled policies, feedback, child-specific queries, and commit failure followed by recovery from the settled original.
- Frontend suite: **302 passed across 40 files**, including preservation of other model settings and multiline literal entry. TypeScript/Vite and the native binary build passed.
- Native WebView fixture: actual Settings, workflow selection, Validate, Save and Run; mock provider verifies extraction before admission, exact recovery of an omitted fact, unchanged prior tool content, retrieval bypass, context usage, manual compaction, restart and fork. The separate native compaction fixture also passes manual summaries, typed provider-overflow recovery, restored workspace rules, restart and fork.

Local native evidence is retained under `desktop/src-tauri/target/native-compression-1788875638758` and `desktop/src-tauri/target/native-compaction-1788875160987` (reports, provider requests, screenshots and app logs). These isolated test profiles are build artifacts, not application Settings or committed user data.

Reproduce from the repository root:

```powershell
cargo test -p aworkit-capability-host --lib
cargo test --manifest-path desktop/src-tauri/Cargo.toml --lib -- --test-threads=4
cargo run -p aworkit-capability-host --example context_compression_benchmark --release
```

From `desktop`, run `npm test -- src/ --maxWorkers=1 --testTimeout=60000`, `npm run build`, and `node scripts/native-compression-smoke.mjs`. Set `AWORKIT_QA_BINARY` to the rebuilt executable when using a separate target directory. The native fixture creates its own profile, source files and local provider, captures requests/screenshots/report, and stops only its own app process.

The [reproducible benchmark output](context-compression-benchmark.json) uses 21 warm release iterations and `o200k_base`, with complete representation overhead included:

| Fixture | Mode used | Tokens before → after | Reduction | Median / p95 local time |
| --- | --- | --- | --- | --- |
| 500 JSON event rows | Lossless | 14,004 → 3,627 | 74.1% | 5.36 / 5.85 ms |
| 800 log records | Lossless | 19,200 → 4,964 | 74.1% | 8.73 / 9.27 ms |
| Source exploration | Adaptive | 8,460 → 1,202 | 85.8% | 4.32 / 4.60 ms |
| Retrieved prose | Adaptive | 2,517 → 1,738 | 30.9% | 3.20 / 3.38 ms |

Lossless mode leaves the source/prose fixtures unchanged. Adaptive mode chooses the same reversible representation for the JSON/log fixtures. These are synthetic transformation measurements, not a corpus-wide answer-quality or end-to-end billing benchmark. First tokenizer initialization is excluded from the warm timings; the native debug test exercises that cold path as well. No assertion of universal answer-quality parity or guaranteed API savings follows from these numbers.
