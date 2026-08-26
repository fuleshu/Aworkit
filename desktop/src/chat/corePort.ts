import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";
import type {
  ChatIntent,
  ChatProjectChoice,
  ChatProjection,
  CoreEventEnvelope,
  EvidenceRecord,
} from "./types";

const chatProjectionSchema = z.object({
  chatId: z.string(),
  runId: z.string(),
  title: z.string(),
  scope: z.string(),
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
const runtimeSnapshotSchema = z.object({
  version: z.number().int().nonnegative(),
  throughSequence: z.number().int().nonnegative(),
  reducerVersion: z.string().min(1),
  stateHash: z.string().startsWith("sha256:"),
  chat: chatProjectionSchema,
  projects: z.array(chatProjectChoiceSchema),
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
  readonly throughSequence: number;
  readonly reducerVersion: string;
  readonly stateHash: string;
  readonly chat: ChatProjection;
  readonly projects: readonly ChatProjectChoice[];
  readonly evidence: readonly EvidenceRecord[];
  readonly events: readonly RuntimeEvent[];
}
export type RuntimeEvent = CoreEventEnvelope;
export interface RuntimeReceipt {
  readonly commandId: string;
  readonly accepted: boolean;
  readonly currentVersion: number;
  readonly reason: string | null;
}
export interface ChatCorePort {
  snapshot(afterSequence: number): Promise<RuntimeSnapshot>;
  command(intent: ChatIntent, expectedVersion: number): Promise<RuntimeReceipt>;
  subscribeEvents?(
    listener: (event: CoreEventEnvelope) => void,
  ): Promise<() => void>;
}

export function normalizeRuntimeSnapshot(input: unknown): RuntimeSnapshot {
  const parsed = runtimeSnapshotSchema.parse(input);
  return {
    version: parsed.version,
    throughSequence: parsed.throughSequence,
    reducerVersion: parsed.reducerVersion,
    stateHash: parsed.stateHash,
    chat: {
      ...parsed.chat,
      phase: parsed.chat.phase,
      disabledReason: parsed.chat.disabledReason ?? undefined,
    },
    projects: parsed.projects,
    evidence: parsed.evidence.map((item) => ({
      ...item,
      category: knownCategory(item.category),
      state: knownEvidenceState(item.state),
    })),
    events: parsed.events,
  };
}

/** Projects a typed renderer intent into the exact native IPC payload. */
export function chatIntentPayload(intent: ChatIntent): unknown {
  if (intent.type === "start")
    return {
      workflowId: intent.workflowId,
      projectId: intent.projectId,
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
  public async snapshot(afterSequence = 0): Promise<RuntimeSnapshot> {
    return {
      version: this.version,
      throughSequence: this.version,
      reducerVersion: "chat.semantic.reducer.v1",
      stateHash: `sha256:${"0".repeat(64)}`,
      chat: this.chat,
      projects: [],
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
      this.chat = {
        chatId: intent.commandId,
        runId: "run.draft",
        title: "New Chat",
        scope: "No project",
        workflowName: null,
        branch: null,
        projectId: null,
        phase: "draft",
        lockedWorkflow: false,
        recoveryPending: false,
        queuedInputs: [],
        expectedVersion: this.version,
      };
    }
    if (intent.type === "pause") this.chat = { ...this.chat, phase: "paused" };
    if (intent.type === "resume")
      this.chat = { ...this.chat, phase: "running", recoveryPending: false };
    if (intent.type === "abandon_recovery")
      this.chat = { ...this.chat, phase: "failed", recoveryPending: false };
    if (intent.type === "cancel")
      this.chat = { ...this.chat, phase: "cancelled" };
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
