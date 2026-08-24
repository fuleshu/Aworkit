import {
  ProjectionGateway,
  type CoreEvent,
  type ProjectionReducer,
} from "../workbench/projection";
import type { ChatIntent, ChatProjection, TimelineItem } from "./types";

export interface ChatWorkspaceModel {
  readonly chat: ChatProjection;
  readonly timeline: readonly TimelineItem[];
}

const initialChat: ChatProjection = {
  chatId: "draft-chat",
  runId: "draft-run",
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
export const initialWorkspace: ChatWorkspaceModel = {
  chat: initialChat,
  timeline: [],
};

/** Reduces only core-provided Chat projection events into an immutable screen model. */
export const chatProjectionReducer: ProjectionReducer<ChatWorkspaceModel> = {
  initial: initialWorkspace,
  reduce(model, event) {
    const payload = event.payload as {
      chat?: Partial<ChatProjection>;
      item?: TimelineItem;
    };
    if (event.kind === "chat.updated" && payload.chat !== undefined)
      return { ...model, chat: { ...model.chat, ...payload.chat } };
    if (event.kind === "timeline.append" && payload.item !== undefined)
      return { ...model, timeline: [...model.timeline, payload.item] };
    return model;
  },
};

/** Keeps the last contiguous view frozen when the core stream has a sequence gap. */
export class ChatWorkspaceController {
  private readonly gateway = new ProjectionGateway(chatProjectionReducer);
  private selectedTimelineId: string | null = null;
  public receive(event: CoreEvent): void {
    this.gateway.receiveEvent(event);
  }
  public snapshot(): ChatWorkspaceModel {
    return this.gateway.snapshot().model;
  }
  public isStale(): boolean {
    return this.gateway.snapshot().stale;
  }
  public selectEvidence(itemId: string): void {
    this.selectedTimelineId = itemId;
  }
  public selectedEvidence(): string | null {
    return this.selectedTimelineId;
  }
  public createIntent(type: ChatIntent["type"], targetId?: string): ChatIntent {
    const commandId = this.gateway.createCommandId("chat");
    if (type === "start")
      return {
        type,
        commandId,
        workflowId: "",
        projectId: null,
        input: "",
        attachments: [],
      };
    if (type === "enqueue") return { type, commandId, input: "" };
    if (type === "approval")
      return { type, commandId, targetId: targetId ?? "", approved: false };
    return { type, commandId, targetId };
  }
  public resynchronize(sequence: number, model: ChatWorkspaceModel): void {
    this.gateway.resynchronize(sequence, model);
  }
}
