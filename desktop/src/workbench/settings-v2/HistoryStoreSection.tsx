import { useCallback, useEffect, useRef, useState } from "react";
import type {
  HistoryReclaimOutcome,
  HistoryReclaimProgress,
  HistoryStoreStatus,
} from "../settingsV2Port";

/** Human units for a measured byte count, one decimal, e.g. `30.8 GB`. */
function formatBytes(bytes: number): string {
  const units = ["B", "KB", "MB", "GB"] as const;
  let value = Math.max(0, bytes);
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}

function formatCount(value: number): string {
  return value.toLocaleString("en-US");
}

function failureMessage(failure: unknown): string {
  return failure instanceof Error ? failure.message : String(failure);
}

const RECLAIM_QUESTION = "Reclaim history store space?";

/** The stated cost of the pass, before the user starts it. */
function reclaimBody(releaseableBytes: number): string {
  return (
    `Aworkit will release at most ${formatBytes(releaseableBytes)} of superseded ` +
    "context snapshots and model-call request bodies, remove the stored events " +
    "of Chats you already deleted, and drop the delivered copies its delivery " +
    "queue still holds. The pass rewrites the whole store file, so it can take " +
    "several minutes, and Aworkit cannot do other work until it finishes."
  );
}

function progressLabel(progress: HistoryReclaimProgress): string {
  if (progress.phase === "rewriting")
    return "Rewriting the store file… this is the slow part";
  return progress.total > 0
    ? `Releasing superseded snapshots — ${formatCount(progress.done)} of ${formatCount(progress.total)} Chats`
    : "Releasing superseded snapshots…";
}

/**
 * What the local history store holds, what a Chat keeps, and the one explicit
 * action that returns superseded bytes to the operating system. The policy is
 * stated here because the store is otherwise invisible; the action never runs
 * on its own, and it states its cost before it starts.
 */
export function HistoryStoreSection({
  loadStatus,
  reclaim,
  onProgress,
  confirm,
}: {
  readonly loadStatus: () => Promise<HistoryStoreStatus>;
  readonly reclaim: () => Promise<HistoryReclaimOutcome>;
  readonly onProgress: (
    handler: (progress: HistoryReclaimProgress) => void,
  ) => Promise<() => void>;
  /** The app's confirm path; without it the panel confirms in place. */
  readonly confirm?: (title: string, body: string) => Promise<boolean>;
}): React.JSX.Element {
  const [status, setStatus] = useState<HistoryStoreStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [asking, setAsking] = useState(false);
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState<HistoryReclaimProgress | null>(null);
  const [outcome, setOutcome] = useState<HistoryReclaimOutcome | null>(null);
  const [reclaimError, setReclaimError] = useState<string | null>(null);
  // Both props are inline arrows at the call site. The one measurement and the
  // one subscription happen when the section mounts, so they read the current
  // props through refs instead of re-running on every parent render.
  const callbacks = useRef({ loadStatus, onProgress });
  useEffect(() => {
    callbacks.current = { loadStatus, onProgress };
  }, [loadStatus, onProgress]);

  const measure = useCallback(async () => {
    setLoading(true);
    try {
      const measured = await callbacks.current.loadStatus();
      setStatus(measured);
      setLoadError(null);
    } catch (failure) {
      setLoadError(
        `The history store could not be measured: ${failureMessage(failure)}`,
      );
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    let current = true;
    let unlisten: (() => void) | null = null;
    void measure();
    void callbacks.current
      .onProgress((next) => {
        if (current) setProgress(next);
      })
      .then((dispose) => {
        if (current) unlisten = dispose;
        else dispose();
      })
      .catch(() => {
        // Progress is a report, never a precondition: the pass still returns
        // its result when this build cannot subscribe to the event.
      });
    return () => {
      current = false;
      unlisten?.();
    };
  }, [measure]);

  const runReclaim = async () => {
    setConfirming(false);
    setProgress(null);
    setOutcome(null);
    setReclaimError(null);
    setRunning(true);
    try {
      const result = await reclaim();
      setOutcome(result);
      // A release changes what the store holds, so the measured line is read
      // again instead of being adjusted by hand.
      await measure();
    } catch (failure) {
      setReclaimError(failureMessage(failure));
    } finally {
      setRunning(false);
    }
  };

  const releaseableBytes =
    (status?.snapshotBytes ?? 0) +
    (status?.deletedChatBytes ?? 0) +
    // Delivered delivery records are removed outright, so their payload bytes
    // belong in the stated ceiling as well.
    (status?.outboxBytes ?? 0);

  const requestReclaim = async () => {
    setReclaimError(null);
    setOutcome(null);
    if (confirm === undefined) {
      setConfirming(true);
      return;
    }
    setAsking(true);
    try {
      if (await confirm(RECLAIM_QUESTION, reclaimBody(releaseableBytes)))
        await runReclaim();
    } catch (failure) {
      setReclaimError(failureMessage(failure));
    } finally {
      setAsking(false);
    }
  };

  const reclaimTitle = running
    ? "A reclaim pass is already running; Aworkit cannot do other work until it finishes"
    : asking
      ? "Waiting for your confirmation before anything is released"
      : status === null
        ? "The store must be measured before it can be reclaimed"
        : "Release superseded snapshots, the events of deleted Chats and the delivered copies its delivery queue holds, then rewrite the store file so the space returns to the operating system";

  return (
    <div className="settings-section-stack">
      <h3>History store</h3>
      <p className="section-intro">
        The local history store is the only active Chat-history backend in this
        build. It is measured here because its cost is otherwise invisible, and
        its retention is bounded: a Chat keeps what it needs to continue.
      </p>
      {loadError !== null && (
        <p className="field-error" role="alert">
          {loadError}
        </p>
      )}
      {status === null ? (
        <p className="settings-field-help" role="status" aria-live="polite">
          {loading ? "Measuring the history store…" : "The history store is not measured."}
        </p>
      ) : (
        <>
          <dl className="history-store-measurements">
            <div>
              <dt>Store file on disk</dt>
              <dd>{formatBytes(status.storeBytes)}</dd>
            </div>
            <div>
              <dt>Stored payload</dt>
              <dd>{formatBytes(status.payloadBytes)}</dd>
            </div>
            <div>
              <dt>Superseded snapshots and request bodies</dt>
              <dd>{formatBytes(status.snapshotBytes)}</dd>
            </div>
            <div>
              <dt>Deleted Chats still stored</dt>
              <dd>
                {formatCount(status.deletedChats)} holding{" "}
                {formatBytes(status.deletedChatBytes)}
              </dd>
            </div>
            {status.outboxRows > 0 && (
              <div>
                <dt>Delivery records still queued</dt>
                <dd>
                  {formatCount(status.outboxRows)} holding{" "}
                  {formatBytes(status.outboxBytes)};{" "}
                  {formatCount(status.outboxDeliveredRows)} already delivered
                </dd>
              </div>
            )}
          </dl>
          {status.outboxRows > 0 && (
            <p className="settings-field-help">
              The delivery queue between Aworkit's core and its window keeps a
              copy of every event it has already delivered, and a delivered copy
              is redundant because the event itself is stored.
            </p>
          )}
          <p className="settings-field-help">
            A reclaim draws on at most {formatBytes(releaseableBytes)}: the
            superseded payload above, the bytes of Chats you already deleted, and
            the delivery copies its queue no longer needs.
          </p>
          <p className="settings-field-help">
            A Chat always keeps the newest snapshot of every context scope and
            its newest {formatCount(status.retainedTurns)} turns, which is what
            lets it continue where it ended.
          </p>
          {status.kinds.length === 0 ? (
            <p className="settings-empty">The store holds no payload yet.</p>
          ) : (
            <table className="history-store-kinds">
              <caption>
                Stored payload by event kind, largest first. Only the superseded
                snapshots and request bodies inside the kinds above are ever
                released; every other stored kind is kept whole. The delivery
                queue is accounted for separately above.
              </caption>
              <thead>
                <tr>
                  <th scope="col">Event kind</th>
                  <th scope="col">Events</th>
                  <th scope="col">Payload</th>
                </tr>
              </thead>
              <tbody>
                {status.kinds.map((entry) => (
                  <tr key={entry.kind}>
                    <th scope="row">{entry.kind}</th>
                    <td>{formatCount(entry.events)}</td>
                    <td title={`${formatCount(entry.bytes)} bytes`}>
                      {formatBytes(entry.bytes)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          <div className="history-store-policy">
            <p>
              <strong>Kept, always:</strong> the whole conversation, tool call
              records and their results, usage and timing, and the sidebar entry
              of every Chat including deleted ones. Per Chat, Aworkit also keeps
              the newest snapshot of every context scope plus its newest{" "}
              {formatCount(status.retainedTurns)} turns, which is what lets a
              Chat continue where it ended.
            </p>
            <p>
              <strong>Released:</strong> only superseded context snapshots and
              superseded model-call request bodies. Each of those is replaced by
              a stated marker carrying the byte count and a digest of what was
              dropped, so the timeline and Run details say “not retained”
              instead of silently showing less. Already-delivered delivery
              records are removed rather than marked, because each one is a
              redundant copy of an event that is still stored.
            </p>
            <p>
              <strong>Deleted Chats:</strong> their events are removed, because
              you already deleted them. Their sidebar entry stays, so the
              deletion itself remains visible. Nothing here is silent: no
              release happens until you ask for one.
            </p>
          </div>
        </>
      )}
      <div className="provider-actions">
        <button
          disabled={running || asking || status === null}
          title={reclaimTitle}
          type="button"
          onClick={() => void requestReclaim()}
        >
          {running ? "Reclaiming…" : "Reclaim space…"}
        </button>
        <button
          disabled={running || loading}
          title="Measure the history store again without changing it"
          type="button"
          onClick={() => void measure()}
        >
          Measure again
        </button>
      </div>
      {confirming && (
        <section
          aria-labelledby="history-store-reclaim-heading"
          className="history-store-confirm"
        >
          <h4 id="history-store-reclaim-heading">{RECLAIM_QUESTION}</h4>
          <p>{reclaimBody(releaseableBytes)}</p>
          <div className="provider-actions">
            <button
              title={`Release at most ${formatBytes(releaseableBytes)} and rewrite the store file now`}
              type="button"
              onClick={() => void runReclaim()}
            >
              Reclaim space
            </button>
            <button
              title="Leave the history store exactly as it is"
              type="button"
              onClick={() => setConfirming(false)}
            >
              Keep everything
            </button>
          </div>
        </section>
      )}
      {running && (
        <p
          className="settings-field-help history-store-progress"
          role="status"
          aria-live="polite"
        >
          {progress === null
            ? "Starting the reclaim pass…"
            : progressLabel(progress)}
        </p>
      )}
      {outcome !== null && (
        <div className="history-store-result" role="status" aria-live="polite">
          <p>
            Released {formatBytes(outcome.report.payloadBytesReleased)} from{" "}
            {formatCount(outcome.report.payloadsPruned)} superseded payloads and
            removed {formatCount(outcome.report.eventsRemoved)} events from{" "}
            {formatCount(outcome.report.chatsPurged)} deleted Chats.
          </p>
          {outcome.report.outboxRowsRemoved > 0 && (
            <p>
              Removed {formatCount(outcome.report.outboxRowsRemoved)}{" "}
              already-delivered records from the delivery queue.
            </p>
          )}
          {outcome.report.outboxBytesReleased > 0 && (
            <p>
              The removed delivery copies held{" "}
              {formatBytes(outcome.report.outboxBytesReleased)}.
            </p>
          )}
          <p>
            Store size {formatBytes(outcome.storeBytesBefore)} →{" "}
            {formatBytes(outcome.storeBytesAfter)}.
          </p>
        </div>
      )}
      {reclaimError !== null && (
        <p className="field-error" role="alert">
          {`The reclaim pass did not finish: ${reclaimError}`}
        </p>
      )}
    </div>
  );
}
