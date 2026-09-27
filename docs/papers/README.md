# Research references: agent context management

Third-party papers kept here so the compaction design can be checked against the
literature without a network round trip. Every file is the authors' own PDF,
unmodified; the licence and the canonical URL are recorded below and attribution
is in the file itself. These are references, not Aworkit documents: nothing here
overrides [docs/context-compaction.md](../context-compaction.md) or the Adashi
specification `aworkit.workflow_worker.context_compaction_budget`.

Downloaded 2026-09-26. The work they support is task 149, "A checkpoint written
by code instead of a decoded summary: index, extract, no model call".

## The papers

| file | paper | venue | licence | why it is here |
| --- | --- | --- | --- | --- |
| [2025-complexity-trap-observation-masking-2508.21433.pdf](2025-complexity-trap-observation-masking-2508.21433.pdf) | The Complexity Trap: Simple Observation Masking Is as Efficient as LLM Summarization for Agent Context Management | DL4C workshop @ NeurIPS 2025 (arXiv 2508.21433v3) | CC BY 4.0 | The controlled comparison our strategy follows: masking matches LLM summarization on SWE-bench Verified at about half the cost |
| [2026-agora-inference-free-agent-compression-2605.26596.pdf](2026-agora-inference-free-agent-compression-2605.26596.pdf) | AGORA: Adapter-Grounded Observation-Action Retention for Inference-Free Prompt Compression in LLM Agents | arXiv 2605.26596v1 | CC BY 4.0 | Why token-level compression is unsafe on agent prompts, and what a step-level, no-model-call compressor looks like |
| [2026-arc-active-context-management-2601.12030.pdf](2026-arc-active-context-management-2601.12030.pdf) | ARC: Active and Reflection-driven Context Management for Long-Horizon Information Seeking Agents | arXiv 2601.12030v1 | CC BY 4.0 | Names "context rot" and measures active revision against passive compression |
| [2026-agent-memory-survey-2603.07670.pdf](2026-agent-memory-survey-2603.07670.pdf) | Memory for Autonomous LLM Agents: Mechanisms, Evaluation, and Emerging Frontiers | arXiv 2603.07670v1 | CC BY 4.0 | Taxonomy and evaluation survey; places a code-written checkpoint among the known mechanism families |
| [2024-longllmlingua-prompt-compression-acl-long.91.pdf](2024-longllmlingua-prompt-compression-acl-long.91.pdf) | LongLLMLingua: Accelerating and Enhancing LLMs in Long Context Scenarios via Prompt Compression | ACL 2024 (2024.acl-long.91) | CC BY 4.0 | The token-pruning family that AGORA measures failing on agents |

## What each one constrains in our design

### The Complexity Trap — masking is a first-class strategy, not a fallback

- Both masking and summarization more than halve cost against the raw agent, and
  **masking matches, and sometimes slightly exceeds, summarization's solve rate**.
  With Qwen3-Coder-480B: 52% cheaper than the raw agent *and* +2.6% solve rate,
  and $0.03 per instance cheaper than summarization ($15 over 500 instances).
- **Masking keeps the full reasoning and action chain** and replaces only
  environment observations older than a window with a placeholder. Consequence
  for us: compress observations, not the narrative — a folded span should keep
  each dropped turn's own visible text and a bounded reasoning head, and index or
  mask its observations.
- Their **hybrid (masking plus a summary) is a further 7% cheaper than masking
  and 11% cheaper than summarization**. Consequence: the model-written summary is
  an enrichment layer on top of masking, not the mechanism, so a small cap is the
  right shape (a low `summaryShare`, or a declared model output).
- They report a **trajectory-elongation effect**: summarization can make
  trajectories *longer*. A summarization defect can therefore look like a wall
  clock regression rather than a token regression, which is the shape of the
  measured 27B run (296,555 output tokens and unfinished at 100 minutes).
- Related work they cite: a concurrent paper's "Delete" baseline (whole turns, no
  model call) was more efficient than summarization at comparable performance,
  and another line uses observation masking for RL training of deep-research and
  computer-use agents.

### AGORA — never compress inside an action

- Every one of 17 (env, backbone, method) cells using **token-level extractive
  compressors collapsed to mean reward ≤ 0.05** despite 1.3–13.3× realized
  compression. They name the cause **action-grammar destruction**: the tokens
  carrying action semantics (identifiers, brackets, action verbs) are exactly the
  ones self-information ranks lowest, so a general-purpose compressor removes
  them and the environment rejects what is left.
- Their remedy is **step granularity**: a structural prompt parser, an
  **always-keep floor** for format- and recency-critical content, and a
  125M-parameter relevance scorer (~2 ms/step, zero per-step LLM toll). The
  ablation finds the **structural floor is the dominant quality lever**, not the
  learned scorer, so most of the benefit is available without a model.
- Our implementation already obeys the rule this paper measures: pruning never
  touches assistant tool calls, only tool *results* (head + tail with an explicit
  marker), and a compaction only ever removes whole units. It is written down
  here so nobody "improves" that into a token-level compressor.

### ARC — the failure has a name

- **Context rot**: performance degrades as interaction history grows, and the
  paper argues against both raw accumulation and *passive* summarization, in
  favour of active, reflection-driven revision of the working context. Their
  mechanism is still model-driven, so it is not our path, but it is the state of
  the art on the other side of the question and it is the strongest statement
  that passive summarization is not the answer.

### The memory survey — where a code-written checkpoint sits

- Formalises agent memory as a **write–manage–read loop** and classifies the
  mechanisms into context-resident compression, retrieval-augmented stores,
  reflective self-improvement, hierarchical virtual context, and policy-learned
  management. Our deterministic state block plus a record-derived ledger is
  context-resident compression whose *manage* policy is not model-driven; the
  durable originals and their retrieval are the second family, already present.
- Useful for evaluating the design later: the survey traces the shift from static
  recall benchmarks to multi-session agentic tests, which is where a checkpoint
  strategy should eventually be judged.

### LongLLMLingua — the token-pruning baseline and its limits

- Perplexity-based token pruning compresses prompts effectively for long-context
  question answering, and it is the family AGORA shows does not transfer to agent
  loops. Kept as the reference for the token-level option, and as the thing *not*
  to do to action grammar.

## Production references (linked, not redistributed)

- Anthropic, **Context editing** — server-side `clear_tool_uses_20250919` (clear
  the oldest tool results in chronological order, replace each with placeholder
  text, optionally clear tool inputs too) and `clear_thinking_20251015` (clear
  thinking blocks under a `keep` policy), both applied with no model call:
  <https://platform.claude.com/docs/en/build-with-claude/context-editing>
- Anthropic, **Effective context engineering for AI agents**:
  <https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents>

Their documentation is not redistributed here: it is copyrighted product
documentation, and only its behaviour is described above.

## Refreshing

Canonical URLs, all reachable without authentication:

- <https://arxiv.org/pdf/2508.21433>
- <https://arxiv.org/pdf/2605.26596>
- <https://arxiv.org/pdf/2601.12030>
- <https://arxiv.org/pdf/2603.07670>
- <https://aclanthology.org/2024.acl-long.91.pdf>

A newer arXiv version keeps the same identifier: replace the file, and update the
version suffix in the table above. Check the licence on the abstract page before
replacing a file, and keep the attribution and the licence note in the table
accurate.
