import { invoke } from "@tauri-apps/api/core";
import { evaluateActivationGate, latestCapabilityReport } from "./repair";
import { createManagementRepairPreviewProjection } from "./preview";
import type {
  ManagementRepairCommandV1,
  ManagementRepairProjectionV1,
  ManagementRepairReceiptV1,
  RepairEventV1,
} from "./types";

export interface ManagementRepairCorePort {
  /** Returns a complete projection plus the ordered event delta. */
  snapshot(afterSequence: number): Promise<ManagementRepairProjectionV1>;
  command(
    command: ManagementRepairCommandV1,
    expectedVersion: number,
  ): Promise<ManagementRepairReceiptV1>;
}

/** Thin native adapter. The trusted core validates all candidate and gate facts. */
export class TauriManagementRepairCorePort
  implements ManagementRepairCorePort
{
  public async snapshot(
    afterSequence: number,
  ): Promise<ManagementRepairProjectionV1> {
    return await invoke<ManagementRepairProjectionV1>(
      "management_repair_snapshot",
      { afterSequence },
    );
  }

  public async command(
    command: ManagementRepairCommandV1,
    expectedVersion: number,
  ): Promise<ManagementRepairReceiptV1> {
    return await invoke<ManagementRepairReceiptV1>(
      "management_repair_command",
      { command, expectedVersion },
    );
  }
}

/**
 * Browser builds have no trusted core and therefore expose no repair facts or
 * commands. Interactive preview data must be injected explicitly by a test or
 * story so it can never be mistaken for a native capability report.
 */
class UnavailableManagementRepairCorePort
  implements ManagementRepairCorePort
{
  public async snapshot(
    _afterSequence: number,
  ): Promise<ManagementRepairProjectionV1> {
    throw new Error(
      "Management repair requires the native trusted-core runtime; browser preview facts are disabled.",
    );
  }

  public async command(
    command: ManagementRepairCommandV1,
    _expectedVersion: number,
  ): Promise<ManagementRepairReceiptV1> {
    return {
      commandId: command.commandId,
      accepted: false,
      currentVersion: 0,
      reason:
        "Management repair commands require the native trusted-core runtime.",
    };
  }
}

/**
 * Browser-safe deterministic port used by the bundled preview and tests. It
 * repeats the decisive server checks so the preview never demonstrates a
 * force-activate path; native deployments still rely on the trusted core.
 */
export class PreviewManagementRepairCorePort
  implements ManagementRepairCorePort
{
  private state: ManagementRepairProjectionV1;
  private history: readonly RepairEventV1[];
  private readonly receipts = new Map<
    string,
    { readonly fingerprint: string; readonly receipt: ManagementRepairReceiptV1 }
  >();

  public constructor(
    initial: ManagementRepairProjectionV1 =
      createManagementRepairPreviewProjection(),
  ) {
    this.history = [...initial.events];
    this.state = { ...initial, events: [] };
  }

  public async snapshot(
    afterSequence: number,
  ): Promise<ManagementRepairProjectionV1> {
    if (!Number.isSafeInteger(afterSequence) || afterSequence < 0)
      throw new Error("repair projection cursor is invalid");
    if (afterSequence > this.state.lastSequence)
      throw new Error("repair projection cursor is ahead of the trusted core");
    return {
      ...this.state,
      events: this.history.filter((event) => event.sequence > afterSequence),
    };
  }

  public async command(
    command: ManagementRepairCommandV1,
    expectedVersion: number,
  ): Promise<ManagementRepairReceiptV1> {
    const fingerprint = JSON.stringify(command);
    const seen = this.receipts.get(command.commandId);
    if (seen !== undefined) {
      if (seen.fingerprint !== fingerprint)
        throw new Error("repair command ID was reused with different content");
      return seen.receipt;
    }
    if (expectedVersion !== this.state.version)
      throw new Error(
        `repair version conflict: expected ${expectedVersion}, actual ${this.state.version}`,
      );

    const rejection = this.validate(command);
    if (rejection !== null) return this.receipt(command, false, rejection);
    this.apply(command);
    return this.receipt(command, true, null);
  }

  private validate(command: ManagementRepairCommandV1): string | null {
    if (!("candidateId" in command)) return null;
    const candidate = this.state.candidates.find(
      ({ id, version }) =>
        id === command.candidateId &&
        version === command.expectedCandidateVersion,
    );
    if (candidate === undefined)
      return "repair candidate version does not exist or is stale";
    if (
      command.type === "request_managed_local_enrollment" &&
      command.expectedArtifactHash !== candidate.artifactHash
    )
      return "candidate artifact hash changed";
    if (command.type !== "activate_repair_and_restart") return null;
    const report = latestCapabilityReport(
      candidate.id,
      candidate.version,
      this.state.capabilityReports,
    );
    const gate = evaluateActivationGate({
      candidate,
      report,
      projectionStale: false,
      explicitDecision: true,
      commandPending: false,
    });
    if (!gate.allowed)
      return `activation gate rejected: ${gate.blockers.join(", ")}`;
    if (this.state.chat.id === null)
      return "Management Chat context is unavailable";
    if (command.expectedCapabilityDigest !== report?.capabilityDigest)
      return "capability generation changed";
    return null;
  }

  private apply(command: ManagementRepairCommandV1): void {
    let next = this.state;
    if (command.type === "investigate_and_fix") {
      next = {
        ...next,
        investigation: {
          id: `INV-${next.version + 1}`,
          errorGroupId: command.errorGroupId,
          state: "running",
          boundedBy: "20 minutes · frozen Management authority",
          startedAt: new Date().toISOString(),
          steps: [
            { id: "reproduce", label: "Reproduce", state: "active" },
            {
              id: "root-cause",
              label: "Isolate root cause",
              state: "pending",
            },
            {
              id: "candidate-tested",
              label: "Test candidate",
              state: "pending",
            },
            { id: "review", label: "Review", state: "pending" },
          ],
        },
      };
    } else if (command.type === "cancel_repair_task") {
      next = {
        ...next,
        investigation:
          next.investigation?.id === command.investigationId
            ? { ...next.investigation, state: "cancelled" }
            : next.investigation,
      };
    } else if (command.type === "reject_candidate") {
      next = {
        ...next,
        candidates: next.candidates.map((candidate) =>
          candidate.id === command.candidateId &&
          candidate.version === command.expectedCandidateVersion
            ? { ...candidate, state: "rejected" as const }
            : candidate,
        ),
      };
    } else if (command.type === "refresh_activation_capability") {
      next = {
        ...next,
        capabilityReports: next.capabilityReports.map((report) =>
          report.candidateId === command.candidateId &&
          report.candidateVersion === command.expectedCandidateVersion
            ? {
                ...report,
                reportVersion: report.reportVersion + 1,
                freshness: "fresh" as const,
                candidateVersion: command.expectedCandidateVersion,
              }
            : report,
        ),
      };
    } else if (command.type === "activate_repair_and_restart") {
      const candidate = next.candidates.find(
        ({ id, version }) =>
          id === command.candidateId &&
          version === command.expectedCandidateVersion,
      )!;
      next = {
        ...next,
        candidates: next.candidates.map((item) =>
          item.id === candidate.id && item.version === candidate.version
            ? { ...item, state: "activating" as const }
            : item,
        ),
        restartRecovery: {
          activationId: `ACT-${next.version + 1}`,
          state: "checkpointed",
          detail:
            "Management Chat checkpoint committed; awaiting authenticated bootstrap handoff.",
          receiptHash: null,
          checkpoint: {
            id: `CHECKPOINT-${next.version + 1}`,
            chatId: next.chat.id!,
            candidateId: candidate.id,
            candidateVersion: candidate.version,
            createdAt: new Date().toISOString(),
          },
        },
      };
    }

    const event: RepairEventV1 = {
      sequence: next.lastSequence + 1,
      kind: command.type,
      occurredAt: new Date().toISOString(),
      subjectId:
        "candidateId" in command ? command.candidateId
        : "errorGroupId" in command ? command.errorGroupId
        : command.investigationId,
    };
    this.history = [...this.history, event];
    this.state = {
      ...next,
      version: next.version + 1,
      lastSequence: event.sequence,
      events: [],
    };
  }

  private receipt(
    command: ManagementRepairCommandV1,
    accepted: boolean,
    reason: string | null,
  ): ManagementRepairReceiptV1 {
    const receipt = {
      commandId: command.commandId,
      accepted,
      currentVersion: this.state.version,
      reason,
    };
    if (accepted)
      this.receipts.set(command.commandId, {
        fingerprint: JSON.stringify(command),
        receipt,
      });
    return receipt;
  }
}

let nextCommandFallback = 1;
export function nextManagementCommandId(): string {
  const nonce =
    typeof globalThis.crypto?.randomUUID === "function"
      ? globalThis.crypto.randomUUID()
      : `${Date.now().toString(36)}.${nextCommandFallback++}`;
  return `desktop.management.${nonce}`;
}

export function createManagementRepairCorePort(): ManagementRepairCorePort {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window
    ? new TauriManagementRepairCorePort()
    : new UnavailableManagementRepairCorePort();
}
