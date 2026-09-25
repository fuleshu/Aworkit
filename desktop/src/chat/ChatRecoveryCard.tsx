import { useRef, useState } from "react";
import type { ChatIntent } from "./types";
import type { ChatRuntimeState } from "./useChatRuntime";
import "./recovery.css";

interface Props {
  readonly chatId: string;
  readonly recoveryPending: boolean;
  readonly runtime: ChatRuntimeState;
  readonly nextCommandId: () => string;
}

/** A Chat-owned, non-modal decision surface; only committed state clears recovery. */
export function ChatRecoveryCard({ chatId, recoveryPending, runtime, nextCommandId }: Props): React.JSX.Element | null {
  const [confirmStop, setConfirmStop] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState<"resume" | "abandon_recovery" | null>(null);
  const locked = useRef(false);
  const action = submitting ?? runtime.recoveryAction;
  if (!recoveryPending && action === null) return null;
  const busy = action !== null;
  const disabled = busy || runtime.loading || runtime.stale || runtime.pendingCommandIds.size > 0;

  async function submit(type: "resume" | "abandon_recovery"): Promise<void> {
    if (locked.current || disabled) return;
    locked.current = true;
    setSubmitting(type);
    setFailure(null);
    setConfirmStop(false);
    // Capture ownership before awaiting anything. Switching Chats never retargets this action.
    const intent: ChatIntent = { type, targetId: chatId, commandId: nextCommandId() };
    try {
      const accepted = await runtime.dispatch(intent);
      if (!accepted) setFailure(type === "resume"
        ? "Couldn’t continue this reply. Try again, or stop it to send a new message."
        : "Couldn’t stop this reply. Please try again.");
    } catch {
      setFailure("Something went wrong. Please try again.");
    } finally {
      locked.current = false;
      setSubmitting(null);
    }
  }

  return (
    <section className="chat-recovery-card" aria-label="Interrupted reply" aria-busy={busy}>
      <div className="chat-recovery-copy">
        <strong>{confirmStop ? "Stop this reply?" : "Reply interrupted"}</strong>
        <p aria-live="polite">{busy
          ? action === "resume" ? "Continuing your reply…" : "Stopping this reply…"
          : confirmStop ? "Changes already made will be kept. You can then send a new message."
          : "Continue where you left off, or stop this reply to send a new message."}</p>
      </div>
      <div className="chat-recovery-actions">
        {runtime.stale ? (
          <button type="button" disabled={busy} title="Reconnect and refresh this Chat" onClick={() => void runtime.resynchronize()}>Reconnect</button>
        ) : confirmStop ? (
          <>
            <button type="button" disabled={disabled} onClick={() => setConfirmStop(false)}>Go back</button>
            <button type="button" className="danger-action" disabled={disabled} onClick={() => void submit("abandon_recovery")}>Stop reply</button>
          </>
        ) : (
          <>
            <button type="button" className="primary-action" disabled={disabled} title="Continue the interrupted reply" onClick={() => void submit("resume")}>{action === "resume" ? "Continuing…" : "Continue reply"}</button>
            <button type="button" disabled={disabled} title="Stop this reply and keep existing changes" onClick={() => setConfirmStop(true)}>{action === "abandon_recovery" ? "Stopping…" : "Stop reply"}</button>
          </>
        )}
      </div>
      {!busy && (failure || runtime.error) && (
        <div className="chat-recovery-error" role="alert">
          <p>{failure ?? "This reply still needs your attention. Try again, or stop it to send a new message."}</p>
          {runtime.error && <details><summary>Show details</summary><p>{runtime.error.message}</p></details>}
        </div>
      )}
    </section>
  );
}
