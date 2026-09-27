import type { ReactNode } from "react";
import "./notice.css";

interface Props {
  /** Accessible name for the notice. */
  readonly label: string;
  /**
   * A hard stop is announced immediately; a decision waits for the user. The
   * severity also picks the card's accent, so a terminal failure can never look
   * like a resumable interruption.
   */
  readonly severity: "decision" | "failure";
  readonly title: ReactNode;
  readonly body: ReactNode;
  /** Announce body changes politely (the decision card narrates its progress). */
  readonly bodyLive?: boolean;
  readonly busy?: boolean;
  readonly actions?: ReactNode;
  readonly detail?: ReactNode;
}

/**
 * The one Chat-level notice card.
 *
 * A resumable interrupted reply and a terminal Run failure differ only by the
 * severity accent and how they are announced, so both render this shell. One
 * shell keeps the two surfaces consistent and replaces the full-width recovery
 * banner the terminal failure used before.
 */
export function ChatNoticeCard({
  label,
  severity,
  title,
  body,
  bodyLive = false,
  busy,
  actions,
  detail,
}: Props): React.JSX.Element {
  return (
    <section
      className={`chat-notice-card chat-notice-card--${severity}`}
      aria-label={label}
      aria-busy={busy}
      role={severity === "failure" ? "alert" : undefined}
    >
      <div className="chat-notice-copy">
        <strong>{title}</strong>
        <p aria-live={bodyLive ? "polite" : undefined}>{body}</p>
      </div>
      {actions !== undefined && <div className="chat-notice-actions">{actions}</div>}
      {detail}
    </section>
  );
}
