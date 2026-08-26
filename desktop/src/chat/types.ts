/** Aworkit-owned, immutable models consumed by the desktop Chat features. */
export type RunPhase =
  | "draft"
  | "running"
  | "waiting_input"
  | "paused"
  | "awaiting_approval"
  | "cancelling"
  | "cancelled"
  | "completed"
  | "failed";

export interface ChatProjection {
  readonly chatId: string;
  readonly runId: string;
  readonly title: string;
  readonly scope: string;
  readonly workflowName: string | null;
  readonly branch: string | null;
  readonly projectId: string | null;
  readonly phase: RunPhase;
  readonly lockedWorkflow: boolean;
  readonly recoveryPending: boolean;
  readonly queuedInputs: readonly string[];
  readonly expectedVersion: number;
  readonly disabledReason?: string;
}

export interface ChatProjectChoice {
  readonly projectId: string;
  readonly name: string;
  readonly workspaceKind:
    | "local_directory"
    | "git_worktree"
    | "container_mount";
}

export type TimelineKind =
  | "message"
  | "thinking"
  | "plan"
  | "step"
  | "model"
  | "tool"
  | "mcp"
  | "plugin"
  | "subagent"
  | "external_agent"
  | "artifact"
  | "approval"
  | "route"
  | "todo"
  | "error"
  | "verification"
  | "repair"
  | "unknown";

export interface TimelineItem {
  readonly id: string;
  readonly kind: TimelineKind;
  readonly title: string;
  readonly body?: string;
  readonly reasoningCategory?: "summary" | "progress" | "source_provided";
  readonly createdAt: string;
  readonly status?: string;
  readonly action?: "approve" | "reject" | "retry" | "fork" | "continue";
  readonly raw?: unknown;
  readonly metadata?: unknown;
  readonly input?: unknown;
  readonly output?: unknown;
}

/** One sequenced transition from the native per-Run event stream. */
export interface LiveChatActivity {
  readonly schemaVersion?: number;
  readonly requestId: string;
  readonly runId: string;
  readonly sequence?: number;
  readonly eventId?: string;
  readonly activityId: string;
  readonly kind:
    | "thinking"
    | "reasoning"
    | "progress"
    | "response"
    | "model_turn"
    | "step"
    | "tool";
  readonly title: string;
  readonly body: string;
  readonly status: string;
  readonly dataMode?: "append" | "replace" | "retain";
  readonly input?: unknown;
  readonly output?: unknown;
  readonly turn?: number;
  readonly nodeId?: string;
  readonly nodeType?: string;
  readonly callId?: string;
  readonly reasoningCategory?: "summary" | "progress" | "source_provided";
  readonly capabilityId?: string;
  /** First transition sequence, retained by the UI reducer. */
  readonly firstSequence?: number;
}

export interface EvidenceRecord {
  readonly id: string;
  readonly category:
    | "provenance"
    | "usage"
    | "routing"
    | "approval"
    | "artifact"
    | "retry"
    | "opacity"
    | "retention"
    | "debug"
    | "unknown";
  readonly label: string;
  readonly value: unknown;
  readonly state:
    | "available"
    | "redacted"
    | "expired"
    | "unsupported"
    | "opaque";
}

export type ChatIntent =
  | {
      readonly type: "start";
      readonly commandId: string;
      readonly workflowId: string;
      readonly projectId: string | null;
      readonly input: string;
      readonly attachments: readonly string[];
    }
  | {
      readonly type: "enqueue";
      readonly commandId: string;
      readonly input: string;
    }
  | {
      readonly type:
        | "new_chat"
        | "pause"
        | "resume"
        | "abandon_recovery"
        | "cancel"
        | "retry"
        | "fork"
        | "continue";
      readonly commandId: string;
      readonly targetId?: string;
    }
  | {
      readonly type: "approval";
      readonly commandId: string;
      readonly targetId: string;
      readonly approved: boolean;
    };
