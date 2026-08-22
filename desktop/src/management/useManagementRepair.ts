import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  createManagementRepairCorePort,
  type ManagementRepairCorePort,
} from "./corePort";
import type {
  ManagementRepairCommandV1,
  ManagementRepairProjectionV1,
} from "./types";

export interface ManagementRepairRuntime {
  readonly snapshot: ManagementRepairProjectionV1 | null;
  readonly stale: boolean;
  readonly loading: boolean;
  readonly error: string | null;
  readonly pendingCommandIds: ReadonlySet<string>;
  dispatch(command: ManagementRepairCommandV1): Promise<boolean>;
  resynchronize(): Promise<boolean>;
}

/** Maintains a contiguous read model; a gap freezes every Management command. */
export function useManagementRepair(
  explicitPort?: ManagementRepairCorePort,
  pollIntervalMs = 2_000,
): ManagementRepairRuntime {
  const port = useMemo(
    () => explicitPort ?? createManagementRepairCorePort(),
    [explicitPort],
  );
  const [snapshot, setSnapshot] =
    useState<ManagementRepairProjectionV1 | null>(null);
  const snapshotRef = useRef<ManagementRepairProjectionV1 | null>(null);
  const uncertainCommandsRef = useRef(
    new Map<string, ManagementRepairCommandV1>(),
  );
  const [stale, setStale] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [pendingCommandIds, setPendingCommandIds] = useState<
    ReadonlySet<string>
  >(new Set());

  const replaceSnapshot = useCallback(
    (next: ManagementRepairProjectionV1) => {
      snapshotRef.current = next;
      setSnapshot(next);
    },
    [],
  );

  const resynchronize = useCallback(async (): Promise<boolean> => {
    try {
      const next = await port.snapshot(0);
      if (
        snapshotRef.current !== null &&
        next.lastSequence < snapshotRef.current.lastSequence
      )
        throw new Error(
          "trusted-core repair snapshot moved behind the last contiguous projection",
        );
      replaceSnapshot(next);
      setStale(false);
      setError(null);
      return true;
    } catch (failure) {
      setStale(true);
      setError(message(failure));
      return false;
    } finally {
      setLoading(false);
    }
  }, [port, replaceSnapshot]);

  const refresh = useCallback(async (): Promise<void> => {
    const current = snapshotRef.current;
    if (current === null) {
      await resynchronize();
      return;
    }
    try {
      const next = await port.snapshot(current.lastSequence);
      if (
        next.lastSequence === current.lastSequence &&
        next.version === current.version
      )
        return;
      let expected = current.lastSequence + 1;
      for (const event of next.events) {
        if (event.sequence !== expected)
          throw new Error(
            `repair projection gap: expected sequence ${expected}, received ${event.sequence}`,
          );
        expected += 1;
      }
      if (expected - 1 !== next.lastSequence)
        throw new Error(
          `repair projection gap: delta ended at ${expected - 1}, snapshot is ${next.lastSequence}`,
        );
      replaceSnapshot(next);
      setStale(false);
      setError(null);
    } catch (failure) {
      setStale(true);
      setError(message(failure));
    }
  }, [port, replaceSnapshot, resynchronize]);

  useEffect(() => {
    void resynchronize();
    const timer = window.setInterval(() => void refresh(), pollIntervalMs);
    return () => window.clearInterval(timer);
  }, [pollIntervalMs, refresh, resynchronize]);

  const dispatch = useCallback(
    async (command: ManagementRepairCommandV1): Promise<boolean> => {
      const current = snapshotRef.current;
      if (current === null || stale) return false;
      const retryKey = commandRetryKey(command);
      const exactCommand =
        uncertainCommandsRef.current.get(retryKey) ?? command;
      setPendingCommandIds((pending) =>
        new Set([...pending, exactCommand.commandId]),
      );
      try {
        const receipt = await port.command(exactCommand, current.version);
        if (receipt.commandId !== exactCommand.commandId)
          throw new Error(
            "trusted-core repair receipt does not match the pending command",
          );
        uncertainCommandsRef.current.delete(retryKey);
        if (!receipt.accepted) {
          const rejection =
            receipt.reason ?? "The trusted core rejected the repair command.";
          setError(rejection);
          if (rejection.includes("version conflict")) await resynchronize();
          return false;
        }
        return await resynchronize();
      } catch (failure) {
        const failureMessage = message(failure);
        if (failureMessage.includes("version conflict")) {
          uncertainCommandsRef.current.delete(retryKey);
          await resynchronize();
        } else {
          // The native side may have committed before transport failed. A
          // subsequent equivalent user action must replay the same ID.
          uncertainCommandsRef.current.set(retryKey, exactCommand);
        }
        setError(failureMessage);
        return false;
      } finally {
        setPendingCommandIds((pending) => {
          const next = new Set(pending);
          next.delete(exactCommand.commandId);
          return next;
        });
      }
    },
    [port, resynchronize, stale],
  );

  return {
    snapshot,
    stale,
    loading,
    error,
    pendingCommandIds,
    dispatch,
    resynchronize,
  };
}

function message(failure: unknown): string {
  return failure instanceof Error ? failure.message : String(failure);
}

function commandRetryKey(command: ManagementRepairCommandV1): string {
  const { commandId: _commandId, ...semanticCommand } = command;
  return JSON.stringify(semanticCommand);
}
