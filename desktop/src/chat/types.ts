/** Aworkit-owned, immutable models consumed by the desktop Chat features. */
export type RunPhase =
  | "draft"
  | "running"
  | "paused"
  | "awaiting_approval"
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
  readonly phase: RunPhase;
  readonly lockedWorkflow: boolean;
  readonly queuedInputs: readonly string[];
  readonly expectedVersion: number;
  readonly disabledReason?: string;
}

export type TimelineKind =
  | "message"
  | "plan"
  | "model"
  | "tool"
  | "mcp"
  | "plugin"
  | "subagent"
  | "external_agent"
  | "artifact"
  | "approval"
  | "route"
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
