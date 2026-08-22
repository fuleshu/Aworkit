import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";
import type {
  ChatIntent,
  ChatProjection,
  EvidenceRecord,
  TimelineItem,
} from "./types";

const chatProjectionSchema = z.object({
  chatId: z.string(),
  runId: z.string(),
  title: z.string(),
  scope: z.string(),
  workflowName: z.string().nullable(),
  branch: z.string().nullable(),
  phase: z.enum([
    "draft",
    "running",
    "waiting_input",
    "awaiting_approval",
    "paused",
    "cancelling",
    "cancelled",
    "completed",
    "failed",
  ]),
  lockedWorkflow: z.boolean(),
  queuedInputs: z.array(z.string()),
  expectedVersion: z.number().int().nonnegative(),
});
const timelineItemSchema = z.object({
  id: z.string(),
  kind: z.string(),
  title: z.string(),
  body: z.string(),
  createdAt: z.string(),
  status: z.string().nullable(),
  action: z.string().nullable(),
  metadata: z.unknown(),
});
const evidenceRecordSchema = z.object({
  id: z.string(),
  category: z.string(),
  label: z.string(),
  state: z.string(),
  value: z.unknown(),
});
const runtimeEventSchema = z
  .object({ sequence: z.number().int().positive() })
  .passthrough();
const runtimeSnapshotSchema = z.object({
  version: z.number().int().nonnegative(),
  lastSequence: z.number().int().nonnegative(),
  chat: chatProjectionSchema,
  timeline: z.array(timelineItemSchema),
  evidence: z.array(evidenceRecordSchema),
  events: z.array(runtimeEventSchema),
});
const receiptSchema = z.object({
  commandId: z.string(),
  accepted: z.boolean(),
  currentVersion: z.number().int().nonnegative(),
  reason: z.string().nullable(),
});

export interface RuntimeSnapshot {
  readonly version: number;
  readonly lastSequence: number;
  readonly chat: ChatProjection;
  readonly timeline: readonly TimelineItem[];
  readonly evidence: readonly EvidenceRecord[];
  readonly events: readonly RuntimeEvent[];
}
export interface RuntimeEvent {
  readonly sequence: number;
  readonly [key: string]: unknown;
}
export interface RuntimeReceipt {
  readonly commandId: string;
  readonly accepted: boolean;
  readonly currentVersion: number;
  readonly reason: string | null;
}
export interface ChatCorePort {
  snapshot(afterSequence: number): Promise<RuntimeSnapshot>;
  command(intent: ChatIntent, expectedVersion: number): Promise<RuntimeReceipt>;
}

export function normalizeRuntimeSnapshot(input: unknown): RuntimeSnapshot {
  const parsed = runtimeSnapshotSchema.parse(input);
  return {
    version: parsed.version,
    lastSequence: parsed.lastSequence,
    chat: {
      ...parsed.chat,
      phase:
        parsed.chat.phase === "waiting_input" ||
        parsed.chat.phase === "cancelling"
          ? "running"
          : parsed.chat.phase,
    },
    timeline: parsed.timeline.map((item) => ({
      ...item,
      kind: knownKind(item.kind),
      status: item.status ?? undefined,
      action: knownAction(item.action),
      raw: item.metadata,
      metadata: item.metadata,
    })),
    evidence: parsed.evidence.map((item) => ({
      ...item,
      category: knownCategory(item.category),
      state: knownEvidenceState(item.state),
    })),
    events: parsed.events,
  };
}

function intentPayload(intent: ChatIntent): unknown {
  if (intent.type === "start")
    return {
      workflowId: intent.workflowId,
      input: intent.input,
      attachments: intent.attachments,
    };
  if (intent.type === "enqueue") return { input: intent.input };
  if (intent.type === "approval") return { approved: intent.approved };
  return {};
}

/** Native implementation: all persistent or privileged actions cross one typed Tauri port. */
export class TauriChatCorePort implements ChatCorePort {
  public async snapshot(afterSequence: number): Promise<RuntimeSnapshot> {
    return normalizeRuntimeSnapshot(
      await invoke("desktop_snapshot", { afterSequence }),
    );
  }
  public async command(
    intent: ChatIntent,
    expectedVersion: number,
  ): Promise<RuntimeReceipt> {
    return receiptSchema.parse(
      await invoke("desktop_command", {
        command: {
          schemaVersion: 1,
          commandId: intent.commandId,
          expectedVersion,
          action: intent.type,
          targetId: "targetId" in intent ? (intent.targetId ?? null) : null,
          payload: intentPayload(intent),
        },
      }),
    );
  }
}

/** Deterministic browser fallback used by Vite previews and component tests. */
export class PreviewChatCorePort implements ChatCorePort {
  private version = 2;
  private readonly seen = new Map<
    string,
    { readonly fingerprint: string; readonly receipt: RuntimeReceipt }
  >();
  private chat: ChatProjection = {
    chatId: "chat.release",
    runId: "run.8f2a",
    title: "Release readiness",
    scope: "Project Atlas",
    workflowName: "Repository Engineer",
    branch: "codex/auth-refresh",
    phase: "running",
    lockedWorkflow: true,
    queuedInputs: ["Review the migration notes too."],
    expectedVersion: 2,
  };
  private timeline: TimelineItem[] = previewTimeline();
  private readonly evidence: EvidenceRecord[] = previewEvidence();
  private readonly events: RuntimeEvent[] = [
    { sequence: 1, kind: "chat.started" },
    { sequence: 2, kind: "timeline.ready" },
  ];
  public async snapshot(afterSequence = 0): Promise<RuntimeSnapshot> {
    return {
      version: this.version,
      lastSequence: this.version,
      chat: this.chat,
      timeline: this.timeline,
      evidence: this.evidence,
      events: this.events.filter((event) => event.sequence > afterSequence),
    };
  }
  public async command(
    intent: ChatIntent,
    expectedVersion: number,
  ): Promise<RuntimeReceipt> {
    const fingerprint = JSON.stringify({ intent, expectedVersion });
    const previous = this.seen.get(intent.commandId);
    if (previous !== undefined) {
      if (previous.fingerprint !== fingerprint)
        throw new Error("desktop command ID was reused with different content");
      return previous.receipt;
    }
    if (expectedVersion !== this.version)
      throw new Error(
        `desktop command version conflict: expected ${expectedVersion}, actual ${this.version}`,
      );
    if (intent.type === "new_chat") {
      this.chat = {
        chatId: intent.commandId,
        runId: "run.draft",
        title: "New Chat",
        scope: "Project Atlas",
        workflowName: null,
        branch: "main",
        phase: "draft",
        lockedWorkflow: false,
        queuedInputs: [],
        expectedVersion: this.version,
      };
      this.timeline = [];
    }
    if (intent.type === "pause") this.chat = { ...this.chat, phase: "paused" };
    if (intent.type === "resume")
      this.chat = { ...this.chat, phase: "running" };
    if (intent.type === "cancel")
      this.chat = { ...this.chat, phase: "cancelled" };
    if (intent.type === "start" || intent.type === "enqueue")
      this.timeline = [
        ...this.timeline,
        {
          id: intent.commandId,
          kind: "message",
          title: "You",
          body: intent.input,
          createdAt: "now",
          status: "queued",
        },
      ];
    if (intent.type === "enqueue")
      this.chat = {
        ...this.chat,
        queuedInputs: [...this.chat.queuedInputs, intent.input],
      };
    this.version += 1;
    this.events.push({ sequence: this.version, kind: `chat.${intent.type}` });
    this.chat = { ...this.chat, expectedVersion: this.version };
    const receipt = {
      commandId: intent.commandId,
      accepted: true,
      currentVersion: this.version,
      reason: null,
    };
    this.seen.set(intent.commandId, { fingerprint, receipt });
    return receipt;
  }
}

export function createChatCorePort(): ChatCorePort {
  return "__TAURI_INTERNALS__" in window
    ? new TauriChatCorePort()
    : new PreviewChatCorePort();
}

function knownKind(value: string): TimelineItem["kind"] {
  return (
    [
      "message",
      "plan",
      "model",
      "tool",
      "mcp",
      "plugin",
      "subagent",
      "external_agent",
      "artifact",
      "approval",
      "route",
      "error",
      "verification",
      "repair",
    ] as const
  ).includes(value as never)
    ? (value as TimelineItem["kind"])
    : "unknown";
}
function knownAction(value: string | null): TimelineItem["action"] {
  return value !== null &&
    (["approve", "reject", "retry", "fork", "continue"] as const).includes(
      value as never,
    )
    ? (value as TimelineItem["action"])
    : undefined;
}
function knownCategory(value: string): EvidenceRecord["category"] {
  return (
    [
      "provenance",
      "usage",
      "routing",
      "approval",
      "artifact",
      "retry",
      "opacity",
      "retention",
      "debug",
    ] as const
  ).includes(value as never)
    ? (value as EvidenceRecord["category"])
    : "unknown";
}
function knownEvidenceState(value: string): EvidenceRecord["state"] {
  return (
    ["available", "redacted", "expired", "unsupported", "opaque"] as const
  ).includes(value as never)
    ? (value as EvidenceRecord["state"])
    : "opaque";
}

function previewTimeline(): TimelineItem[] {
  return [
    {
      id: "message.user.1",
      kind: "message",
      title: "You",
      body: "Check whether the auth refresh branch is ready to merge.",
      createdAt: "12:41",
    },
    {
      id: "plan.1",
      kind: "plan",
      title: "Release readiness plan",
      body: "Inspect changes\nRun workspace tests\nReview migration risk\nSummarize readiness",
      createdAt: "12:41",
      status: "3 of 4 complete",
      metadata: { completed: 3, total: 4 },
    },
    {
      id: "tool.1",
      kind: "tool",
      title: "Shell",
      body: "cargo test --workspace --all-targets",
      createdAt: "12:42",
      status: "completed",
      metadata: { exitCode: 0, tests: 428 },
    },
    {
      id: "message.assistant.1",
      kind: "message",
      title: "Aworkit",
      body: "The branch is ready for review. All tests passed; the remaining item is a manual migration sign-off.",
      createdAt: "12:43",
    },
  ];
}
function previewEvidence(): EvidenceRecord[] {
  return [
    {
      id: "evidence.tool.1",
      category: "provenance",
      label: "Shell invocation",
      state: "available",
      value: {
        command: "cargo test --workspace --all-targets",
        workingDirectory: "/workspace/project-atlas",
        exitCode: 0,
        tests: 428,
      },
    },
    {
      id: "evidence.usage.1",
      category: "usage",
      label: "Usage and cost",
      state: "available",
      value: { inputTokens: 1284, outputTokens: 326, cost: "local / unpriced" },
    },
    {
      id: "evidence.debug.1",
      category: "debug",
      label: "Detailed protocol capture",
      state: "redacted",
      value: null,
    },
    {
      id: "evidence.routing.1",
      category: "routing",
      label: "Frozen route decision",
      state: "available",
      value: { route: "quality", source: "workflow.transition.deep-review" },
    },
    {
      id: "evidence.approval.1",
      category: "approval",
      label: "Approval decision",
      state: "expired",
      value: null,
    },
    {
      id: "evidence.artifact.1",
      category: "artifact",
      label: "Test report artifact",
      state: "available",
      value: { contentId: "sha256:demo", mediaType: "text/plain" },
    },
    {
      id: "evidence.retry.1",
      category: "retry",
      label: "Attempt policy",
      state: "available",
      value: { attempt: 1, retrySafe: true },
    },
    {
      id: "evidence.opacity.1",
      category: "opacity",
      label: "Provider-private reasoning",
      state: "opaque",
      value: null,
    },
    {
      id: "evidence.retention.1",
      category: "retention",
      label: "Detailed capture retention",
      state: "unsupported",
      value: null,
    },
  ];
}
