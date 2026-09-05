/** Presentation-only facts. Actions delegate to existing feature intents. */
export type NotificationSeverity = "action" | "error" | "warning" | "progress" | "success" | "info";
export type NotificationLifetime =
  | { readonly kind: "transient"; readonly durationMs?: number }
  | { readonly kind: "condition"; readonly conditionId: string }
  | { readonly kind: "operation"; readonly operationId: string };

export interface NotificationAction {
  readonly label: string;
  readonly run: () => void;
  readonly disabled?: boolean;
}

export interface NotificationInput {
  readonly summary: string;
  readonly detail?: string;
  readonly severity: NotificationSeverity;
  readonly lifetime: NotificationLifetime;
  readonly source: string;
  readonly route?: string;
  readonly action?: NotificationAction;
}

export interface NotificationRecord extends NotificationInput {
  readonly id: string;
  readonly scope: string;
  readonly occurrence: number;
  readonly createdAt: number;
  readonly expiresAt: number | null;
  readonly acknowledged: boolean;
}

export interface NotificationSnapshot {
  readonly active: readonly NotificationRecord[];
  readonly recent: readonly NotificationRecord[];
}

export type NotificationTiming = "default" | "extended" | "manual";

export const notificationDuration: Record<NotificationSeverity, number> = {
  success: 5_000, info: 8_000, warning: 12_000, error: 15_000,
  action: 15_000, progress: 8_000,
};
