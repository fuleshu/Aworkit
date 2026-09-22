/**
 * Shared Chat test fixtures.
 *
 * Every Chat test needs the same few shapes — one canonical envelope, a Chat
 * projection, a complete snapshot and a port. Building them here keeps each
 * test file about its own subject instead of about `RuntimeSnapshot`
 * boilerplate, and gives the conventions (stream id, branch, envelope fields,
 * chat phase) exactly one place to change.
 */
import type {
  ChatCorePort,
  ChatEventPage,
  ChatEventWindow,
  RuntimeEvent,
  RuntimeSnapshot,
  SubagentChildSummary,
} from "../../chat/corePort";
import type { ChatHistoryEntry, ChatProjection } from "../../chat/types";

/** One canonical envelope. `payload.spanId` becomes the envelope's span id. */
export function runtimeEvent(
  sequence: number,
  kind: string,
  payload: Record<string, unknown> = {},
  options: { readonly streamId?: string; readonly eventId?: string } = {},
): RuntimeEvent {
  return {
    schemaVersion: 1,
    streamId: options.streamId ?? "chat.test",
    branchId: "main",
    sequence,
    eventId: options.eventId ?? `event.${sequence}`,
    kind,
    spanId: typeof payload.spanId === "string" ? payload.spanId : undefined,
    payload,
  };
}

/** A complete Chat projection; override only what the test is about. */
export function chatProjection(
  overrides: Partial<ChatProjection> = {},
): ChatProjection {
  return {
    approvalMode: "ask_for_approval",
    chatId: "chat.test",
    runId: "run.test",
    title: "Test Chat",
    scope: "No project",
    workflowId: null,
    workflowName: null,
    branch: null,
    projectId: null,
    phase: "waiting_input",
    lockedWorkflow: false,
    recoveryPending: false,
    queuedInputs: [],
    expectedVersion: 0,
    ...overrides,
  };
}

export interface ChatSnapshotOptions {
  readonly chat?: Partial<ChatProjection>;
  readonly events?: readonly RuntimeEvent[];
  readonly throughSequence?: number;
  readonly eventWindow?: ChatEventWindow;
  readonly history?: readonly ChatHistoryEntry[];
  readonly subagents?: readonly SubagentChildSummary[];
}

/** A complete snapshot. The head defaults to the last committed event. */
export function chatSnapshot(options: ChatSnapshotOptions = {}): RuntimeSnapshot {
  const events = options.events ?? [];
  const through = options.throughSequence ?? events.at(-1)?.sequence ?? 1;
  return {
    version: through,
    throughSequence: through,
    reducerVersion: "chat.semantic.reducer.v1",
    stateHash: `sha256:${"0".repeat(64)}`,
    chat: chatProjection({ ...options.chat, expectedVersion: through }),
    history: options.history ?? [],
    projects: [],
    evidence: [],
    events: [...events],
    subagents: options.subagents ?? [],
    ...(options.eventWindow === undefined ? {} : { eventWindow: options.eventWindow }),
  };
}

/** One scoped page. The cursor validates against the same window the core uses. */
export function eventPage(
  events: readonly RuntimeEvent[],
  firstSequence: number,
  headSequence = events.at(-1)?.sequence ?? firstSequence,
): ChatEventPage {
  return {
    window: {
      firstSequence,
      lastSequence: events.at(-1)?.sequence ?? firstSequence,
      headSequence,
      hasMore: firstSequence > 1,
      supportingEvents: [],
    },
    events: [...events],
  };
}

/**
 * A port whose uninteresting calls fail loudly, so a test that unexpectedly
 * reads or commands the core reports it instead of silently succeeding.
 */
export function testPort(overrides: Partial<ChatCorePort> = {}): ChatCorePort {
  return {
    async snapshot(): Promise<RuntimeSnapshot> {
      throw new Error("unused snapshot");
    },
    async command(intent) {
      return {
        commandId: intent.commandId,
        accepted: false,
        currentVersion: 1,
        reason: "unused",
      };
    },
    ...overrides,
  };
}
