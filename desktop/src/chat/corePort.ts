import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";
import { approvalModeSchema } from "./approvals";
import type {
  ChatIntent,
  ChatHistoryEntry,
  ChatProjectChoice,
  ChatProjection,
  CoreEventEnvelope,
  EvidenceRecord,
} from "./types";

const chatProjectionSchema = z.object({
  approvalMode: approvalModeSchema.default("ask_for_approval"),
  chatId: z.string(),
  runId: z.string(),
  title: z.string(),
  scope: z.string(),
  workflowId: z.string().nullable(),
  workflowName: z.string().nullable(),
  branch: z.string().nullable(),
  projectId: z.string().nullable(),
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
  recoveryPending: z.boolean().default(false),
  queuedInputs: z.array(z.string()),
  expectedVersion: z.number().int().nonnegative(),
  disabledReason: z.string().nullable().optional(),
  rememberedWorkflowId: z.string().nullable().optional(),
  rememberedProjectId: z.string().nullable().optional(),
});
const chatProjectChoiceSchema = z.object({
  projectId: z.string().min(1),
  name: z.string().min(1),
  workspaceKind: z.enum([
    "local_directory",
    "git_worktree",
    "container_mount",
  ]),
});
const chatHistoryEntrySchema = z.object({
  chatId: z.string().min(1),
  runId: z.string().min(1),
  title: z.string().min(1),
  projectId: z.string().min(1).nullable(),
  projectName: z.string().min(1).nullable(),
  phase: chatProjectionSchema.shape.phase,
  pinned: z.boolean(),
  parentChatId: z.string().min(1).nullable(),
  createdAt: z.string(),
  updatedAt: z.string(),
});
const evidenceRecordSchema = z.object({
  id: z.string(),
  category: z.string(),
  label: z.string(),
  state: z.string(),
  value: z.unknown(),
});
const runtimeEventSchema = z
  .object({
    schemaVersion: z.number().int().positive(),
    streamId: z.string().min(1),
    branchId: z.string().min(1),
    sequence: z.number().int().positive(),
    eventId: z.string().min(1),
    kind: z.string().min(1),
    spanId: z.string().min(1).optional(),
    causationEventId: z.string().min(1).optional(),
    payload: z.unknown(),
  })
  .strict();
const eventWindowSchema = z.object({
  firstSequence: z.number().int().positive(), lastSequence: z.number().int().nonnegative(),
  headSequence: z.number().int().nonnegative(), hasMore: z.boolean(),
  supportingEvents: z.array(runtimeEventSchema),
});
const eventPageSchema = z.object({ window: eventWindowSchema, events: z.array(runtimeEventSchema) });
export type ChatEventWindow = z.infer<typeof eventWindowSchema>;
export type ChatEventPage = z.infer<typeof eventPageSchema>;
const subagentChildStatusSchema = z.enum([
  "running",
  "completed",
  "parent_approval_required",
  "failed",
  "interrupted",
  "cancelled",
]);
const subagentChildSummarySchema = z.object({
  childId: z.string().min(1),
  kind: z.enum(["fresh", "fork"]),
  status: subagentChildStatusSchema,
  running: z.boolean(),
  depth: z.number().int().nonnegative(),
  nodeId: z.string(),
  parentInvocationId: z.string(),
  parentCallId: z.string().default(""),
  parentChildId: z.string().nullable().optional(),
  task: z.string(),
  contextText: z.string().default(""),
  finalText: z.string().default(""),
  modelTurns: z.number().int().nonnegative(),
  toolCalls: z.number().int().nonnegative(),
  inputTokens: z.number().int().nonnegative(),
  outputTokens: z.number().int().nonnegative(),
  headRevision: z.number().int().nonnegative(),
  createdAt: z.string(),
  updatedAt: z.string(),
});
export type SubagentChildStatus = z.infer<typeof subagentChildStatusSchema>;
export type SubagentChildSummary = z.infer<typeof subagentChildSummarySchema>;
/** A child status is terminal once it can no longer run without new work. */
export function isTerminalSubagentStatus(status: SubagentChildStatus): boolean {
  return status !== "running";
}

const runtimeSnapshotSchema = z.object({
  eventWindow: eventWindowSchema.optional(),
  activeChatIds: z.array(z.string()).default([]),
  contextModel: z.object({ name: z.string(), contextWindow: z.number().positive().nullable() }).nullable().optional(),
  version: z.number().int().nonnegative(),
  throughSequence: z.number().int().nonnegative(),
  reducerVersion: z.string().min(1),
  stateHash: z.string().startsWith("sha256:"),
  chat: chatProjectionSchema,
  history: z.array(chatHistoryEntrySchema).default([]),
  projects: z.array(chatProjectChoiceSchema),
  evidence: z.array(evidenceRecordSchema),
  events: z.array(runtimeEventSchema),
  subagents: z.array(subagentChildSummarySchema).default([]),
});
const receiptSchema = z.object({
  commandId: z.string(),
  accepted: z.boolean(),
  currentVersion: z.number().int().nonnegative(),
  reason: z.string().nullable(),
});
export interface RuntimeSnapshot {
  readonly eventWindow?: ChatEventWindow;
  readonly activeChatIds?: readonly string[];
  readonly contextModel?: import("./contextProjection").ContextModel | null;
  readonly version: number;
  readonly throughSequence: number;
  readonly reducerVersion: string;
  readonly stateHash: string;
  readonly chat: ChatProjection;
  readonly history: readonly ChatHistoryEntry[];
  readonly projects: readonly ChatProjectChoice[];
  readonly evidence: readonly EvidenceRecord[];
  readonly events: readonly RuntimeEvent[];
  readonly subagents?: readonly SubagentChildSummary[];
}
export type RuntimeEvent = CoreEventEnvelope;
export interface RuntimeReceipt {
  readonly commandId: string;
  readonly accepted: boolean;
  readonly currentVersion: number;
  readonly reason: string | null;
}
export interface ChatCorePort {
  olderEvents?(chatId: string, beforeSequence: number, throughSequence: number): Promise<ChatEventPage>;
  /** One delegated child's own evidence from the same canonical Run history. */
  subagentEvents?(
    chatId: string,
    childId: string,
    beforeSequence: number,
    throughSequence: number,
  ): Promise<ChatEventPage>;
  contextModel?(chatId: string, workflowId: string | null): Promise<import("./contextProjection").ContextModel | null>;
  snapshot(afterSequence: number, chatId?: string): Promise<RuntimeSnapshot>;
  command(intent: ChatIntent, expectedVersion: number): Promise<RuntimeReceipt>;
  subscribeEvents?(
    listener: (event: CoreEventEnvelope) => void,
  ): Promise<() => void>;
}

export function normalizeRuntimeSnapshot(input: unknown): RuntimeSnapshot {
  const parsed = runtimeSnapshotSchema.parse(input);
  return {
    eventWindow: parsed.eventWindow,
    activeChatIds: parsed.activeChatIds,
    contextModel: parsed.contextModel,
    version: parsed.version,
    throughSequence: parsed.throughSequence,
    reducerVersion: parsed.reducerVersion,
    stateHash: parsed.stateHash,
    chat: {
      ...parsed.chat,
      phase: parsed.chat.phase,
      disabledReason: parsed.chat.disabledReason ?? undefined,
    },
    history: parsed.history,
    projects: parsed.projects,
    evidence: parsed.evidence.map((item) => ({
      ...item,
      category: knownCategory(item.category),
      state: knownEvidenceState(item.state),
    })),
    events: parsed.events,
    subagents: parsed.subagents,
  };
}

/** Projects a typed renderer intent into the exact native IPC payload. */
export function chatIntentPayload(intent: ChatIntent): unknown {
  if (intent.type === "compact_context") return { nodeId: intent.nodeId, baseSequence: intent.baseSequence };
  if (intent.type === "edit_context") return { nodeId: intent.nodeId, baseSequence: intent.baseSequence, document: intent.document };
  if (intent.type === "approval_mode") return { mode: intent.mode };
  if (intent.type === "set_goal") return { goal: intent.goal ?? "", clear: intent.goal === null };
  if (intent.type === "start")
    return {
      workflowId: intent.workflowId,
      projectId: intent.projectId,
      input: intent.input,
      attachments: intent.attachments,
    };
  if (intent.type === "enqueue") return { input: intent.input, ...(intent.attachments === undefined ? {} : { attachments: intent.attachments }) };
  if (intent.type === "approval")
    return {
      decisionId: intent.decisionId,
      approved: intent.approved,
      ...(intent.choice === undefined ? {} : { choice: intent.choice }),
      ...(intent.reason === undefined ? {} : { reason: intent.reason }),
      ...(intent.filesystem === undefined ? {} : { filesystem: intent.filesystem }),
    };
  if (intent.type === "set_chat_pinned") return { pinned: intent.pinned };
  if (intent.type === "question")
    return {
      questionId: intent.questionId,
      ...(intent.optionId === undefined ? {} : { optionId: intent.optionId }),
      ...(intent.freeText === undefined ? {} : { freeText: intent.freeText }),
      ...(intent.path === undefined ? {} : { path: intent.path }),
      ...(intent.cancelled === undefined ? {} : { cancelled: intent.cancelled }),
    };
  // An intent that carries a payload can never fall through to an empty object:
  // the core decides every decision from that payload, so silently dropping one
  // leaves a Run parked forever. The payload-less actions are therefore named
  // explicitly and the projector below still has to handle everything else.
  if (isPayloadlessChatIntent(intent)) return {};
  return assertProjectedIntentPayload(intent);
}

/**
 * The Chat intents whose native IPC payload is empty by contract.
 *
 * A question answer is deliberately not one of them: the core reads the answer
 * out of the payload and refuses a command without it.
 */
type PayloadlessChatIntent = Extract<
  ChatIntent,
  {
    readonly type:
      | "new_chat"
      | "pause"
      | "resume"
      | "abandon_recovery"
      | "cancel"
      | "retry"
      | "continue"
      | "select_chat"
      | "delete_chat"
      | "fork";
  }
>;

const PAYLOADLESS_INTENT_TYPES: ReadonlySet<string> = new Set([
  "new_chat",
  "pause",
  "resume",
  "abandon_recovery",
  "cancel",
  "retry",
  "continue",
  "select_chat",
  "delete_chat",
  "fork",
]);

/**
 * Narrows to an intent whose native payload is empty by contract.
 *
 * The declared predicate is what makes `assertProjectedIntentPayload` below a
 * compile-time check: a new Chat intent that carries a payload is neither
 * payload-less nor projected, so the projector stops type-checking until it is.
 */
function isPayloadlessChatIntent(intent: ChatIntent): intent is PayloadlessChatIntent {
  return PAYLOADLESS_INTENT_TYPES.has(
    intent.type as PayloadlessChatIntent["type"],
  );
}

/** Compile-time proof that every Chat intent with a payload is projected. */
function assertProjectedIntentPayload(intent: never): never {
  throw new Error(
    `Chat intent '${(intent as ChatIntent).type}' has no projected native payload`,
  );
}

/** Only Chat-scoped actions participate in the native stale-target check. */
export function chatIntentTargetId(intent: ChatIntent): string | null {
  return "targetId" in intent ? (intent.targetId ?? null) : null;
}

/** Native implementation: all persistent or privileged actions cross one typed Tauri port. */
export class TauriChatCorePort implements ChatCorePort {
  public async contextModel(chatId: string, workflowId: string | null) {
    return z.object({ name: z.string(), contextWindow: z.number().positive().nullable() }).nullable()
      .parse(await invoke("desktop_context_model", { chatId, workflowId }));
  }
  public async snapshot(afterSequence: number, chatId?: string): Promise<RuntimeSnapshot> {
    const snapshot = normalizeRuntimeSnapshot(
      await invoke("desktop_chat_snapshot", { afterSequence, chatId }),
    );
    const events = [...snapshot.events];
    const support = [...(snapshot.eventWindow?.supportingEvents ?? [])];
    let cursor = snapshot.eventWindow?.lastSequence ?? snapshot.throughSequence;
    while (cursor < snapshot.throughSequence) {
      const page = eventPageSchema.parse(await invoke("desktop_chat_events", { chatId: snapshot.chat.chatId, afterSequence: cursor, throughSequence: snapshot.throughSequence }));
      if (!page.events.length || page.events[0].sequence !== cursor + 1) throw new Error("Chat recovery did not advance contiguously");
      events.push(...page.events); support.push(...page.window.supportingEvents);
      cursor = page.window.lastSequence;
    }
    return { ...snapshot, events, ...(snapshot.eventWindow ? { eventWindow: { ...snapshot.eventWindow, lastSequence: cursor, supportingEvents: support } } : {}) };
  }
  public async olderEvents(chatId: string, beforeSequence: number, throughSequence: number): Promise<ChatEventPage> {
    return eventPageSchema.parse(await invoke("desktop_chat_events", { chatId, afterSequence: 0, beforeSequence, throughSequence }));
  }
  public async subagentEvents(
    chatId: string,
    childId: string,
    beforeSequence: number,
    throughSequence: number,
  ): Promise<ChatEventPage> {
    return eventPageSchema.parse(
      await invoke("desktop_chat_events", {
        chatId,
        childId,
        afterSequence: 0,
        beforeSequence,
        throughSequence,
      }),
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
          // Approval decision IDs identify a suspended invocation. They must
          // not cross the separate Chat-target freshness boundary.
          targetId: chatIntentTargetId(intent),
          payload: chatIntentPayload(intent),
        },
      }),
    );
  }

  public async subscribeEvents(
    listener: (event: CoreEventEnvelope) => void,
  ): Promise<() => void> {
    const { listen } = await import("@tauri-apps/api/event");
    return listen<unknown>("aworkit:chat-event", ({ payload }) => {
      const parsed = runtimeEventSchema.safeParse(payload);
      if (parsed.success) {
        listener(parsed.data);
      } else {
        console.error("Rejected invalid canonical chat event envelope", parsed.error);
      }
    });
  }
}

/** Deterministic browser fallback used by Vite previews and component tests. */
export class PreviewChatCorePort implements ChatCorePort {
  private version = 0;
  private readonly seen = new Map<
    string,
    { readonly fingerprint: string; readonly receipt: RuntimeReceipt }
  >();
  private chat: ChatProjection = {
    chatId: "chat.preview",
    runId: "run.draft",
    title: "New Chat",
    scope: "No project",
    workflowId: null,
    workflowName: null,
    branch: null,
    projectId: null,
    phase: "draft",
    lockedWorkflow: false,
    recoveryPending: false,
    queuedInputs: [],
    expectedVersion: 0,
  };
  private readonly evidence: EvidenceRecord[] = [];
  private readonly events: RuntimeEvent[] = [];
  private history: ChatHistoryEntry[] = [
    {
      chatId: "chat.preview",
      runId: "run.draft",
      title: "New Chat",
      projectId: null,
      projectName: null,
      phase: "draft",
      pinned: false,
      parentChatId: null,
      createdAt: "0",
      updatedAt: "0",
    },
  ];
  public async snapshot(afterSequence = 0): Promise<RuntimeSnapshot> {
    return {
      version: this.version,
      throughSequence: this.version,
      reducerVersion: "chat.semantic.reducer.v1",
      stateHash: `sha256:${"0".repeat(64)}`,
      chat: this.chat,
      history: this.history,
      projects: [],
      evidence: this.evidence,
      events: this.events.filter((event) => event.sequence > afterSequence),
      subagents: [],
    };
  }
  public async subagentEvents(
    _chatId: string,
    childId: string,
    beforeSequence: number,
    _throughSequence: number,
  ): Promise<ChatEventPage> {
    const events = this.events.filter(
      (event) =>
        event.sequence < beforeSequence
        && (event.payload as { subagentChildId?: unknown } | null)?.subagentChildId === childId,
    );
    return {
      window: {
        firstSequence: events[0]?.sequence ?? beforeSequence,
        lastSequence: events.at(-1)?.sequence ?? beforeSequence - 1,
        headSequence: this.version,
        hasMore: false,
        supportingEvents: [],
      },
      events,
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
    if (intent.type === "start") {
      const receipt = {
        commandId: intent.commandId,
        accepted: false,
        currentVersion: this.version,
        reason:
          "Workflow execution requires the native desktop runtime; browser Preview did not contact a provider.",
      };
      this.seen.set(intent.commandId, { fingerprint, receipt });
      return receipt;
    }
    if (intent.type === "new_chat") {
      const chatId = intent.commandId.replace(/^desktop\./u, "");
      this.chat = {
        chatId,
        runId: "run.draft",
        title: "New Chat",
        scope: "No project",
        workflowId: null,
        workflowName: null,
        branch: null,
        projectId: null,
        phase: "draft",
        lockedWorkflow: false,
        recoveryPending: false,
        queuedInputs: [],
        expectedVersion: this.version,
      };
      this.history = [
        {
          chatId,
          runId: "run.draft",
          title: "New Chat",
          projectId: null,
          projectName: null,
          phase: "draft",
          pinned: false,
          parentChatId: null,
          createdAt: String(Date.now()),
          updatedAt: String(Date.now()),
        },
        ...this.history,
      ];
    }
    if (intent.type === "select_chat") {
      const selected = this.history.find(({ chatId }) => chatId === intent.targetId);
      if (selected !== undefined) {
        this.chat = {
          ...this.chat,
          chatId: selected.chatId,
          runId: selected.runId,
          title: selected.title,
          projectId: selected.projectId,
          scope: selected.projectName ?? "No project",
          phase: selected.phase,
          lockedWorkflow: selected.phase !== "draft",
        };
      }
    }
    if (intent.type === "approval_mode") this.chat = { ...this.chat, approvalMode: intent.mode };
    if (intent.type === "set_chat_pinned")
      this.history = this.history.map((entry) =>
        entry.chatId === intent.targetId
          ? { ...entry, pinned: intent.pinned }
          : entry,
      );
    if (intent.type === "delete_chat") {
      this.history = this.history.filter(({ chatId }) => chatId !== intent.targetId);
      if (this.chat.chatId === intent.targetId) {
        const fallback = this.history[0];
        this.chat = fallback === undefined
          ? { ...this.chat, chatId: "chat.preview", runId: "run.draft", title: "New Chat" }
          : {
              ...this.chat,
              chatId: fallback.chatId,
              runId: fallback.runId,
              title: fallback.title,
              projectId: fallback.projectId,
              scope: fallback.projectName ?? "No project",
              phase: fallback.phase,
            };
      }
    }
    if (intent.type === "fork") {
      const parent = this.history.find(({ chatId }) => chatId === intent.targetId);
      if (parent !== undefined) {
        const child = {
          ...parent,
          chatId: intent.commandId.replace(/^desktop\./u, ""),
          runId: `${parent.runId}.fork`,
          parentChatId: parent.chatId,
          pinned: false,
          createdAt: String(Date.now()),
          updatedAt: String(Date.now()),
        };
        this.history = [child, ...this.history];
        this.chat = { ...this.chat, chatId: child.chatId, runId: child.runId };
      }
    }
    if (intent.type === "pause") this.chat = { ...this.chat, phase: "paused" };
    if (intent.type === "resume")
      this.chat = { ...this.chat, phase: "running", recoveryPending: false };
    if (intent.type === "abandon_recovery")
      this.chat = { ...this.chat, phase: "failed", recoveryPending: false };
    if (intent.type === "cancel")
      this.chat = { ...this.chat, phase: "waiting_input" };
    if (intent.type === "enqueue")
      this.chat = {
        ...this.chat,
        queuedInputs: [...this.chat.queuedInputs, intent.input],
      };
    this.version += 1;
    this.events.push({
      schemaVersion: 1,
      streamId: this.chat.chatId,
      branchId: "main",
      sequence: this.version,
      eventId: `event.chat.${this.version}`,
      kind: `chat.${intent.type}`,
      payload: {},
    });
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
