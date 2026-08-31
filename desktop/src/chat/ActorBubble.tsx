import type { ReactNode } from "react";
import { MarkdownContent } from "./MarkdownContent";

export type ChatActor = "model" | "subagent";
export type ChatBubbleVariant = "speech" | "thinking";

interface ActorBubbleProps {
  readonly actor: ChatActor;
  readonly ariaLabel: string;
  readonly body: string;
  readonly busy?: boolean;
  readonly children?: ReactNode;
  readonly createdAt?: string;
  readonly onSelect?: () => void;
  readonly reasoningLabel?: string;
  readonly selected?: boolean;
  readonly status?: string;
  readonly title?: string;
  readonly variant: ChatBubbleVariant;
}

/**
 * Visually binds model speech or provider-supplied thought to its actual
 * actor. Speech uses a tail; thought uses two small trailing circles.
 */
export function ActorBubble({
  actor,
  ariaLabel,
  body,
  busy = false,
  children,
  createdAt,
  onSelect,
  reasoningLabel,
  selected = false,
  status,
  title,
  variant,
}: ActorBubbleProps): React.JSX.Element {
  const heading = title === undefined ? null : (
    <>
      <span className="bubble-activity-mark" aria-hidden="true">
        {busy ? "" : status === "completed" ? "✓" : "◇"}
      </span>
      <span className="bubble-heading-copy">
        <strong>{title}</strong>
        {reasoningLabel !== undefined && <small>{reasoningLabel}</small>}
      </span>
      <span className={`status ${status ?? ""}`}>{status ?? "Thinking"}</span>
    </>
  );
  return (
    <article
      aria-busy={busy || undefined}
      aria-label={ariaLabel}
      className={`actor-turn actor-${actor} ${variant}-turn ${selected ? "selected" : ""}`}
    >
      <ActorAvatar actor={actor} />
      <div className={`chat-bubble ${variant}-bubble`}>
        {variant === "speech" && onSelect !== undefined && (
          <button
            className="bubble-details-button"
            title={`Show Run details for ${ariaLabel}`}
            type="button"
            onClick={onSelect}
          >
            Details
          </button>
        )}
        {heading !== null &&
          (onSelect === undefined ? (
            <div className="bubble-heading">{heading}</div>
          ) : (
            <button
              className="bubble-heading"
              type="button"
              title={`Show Run details for ${title}`}
              onClick={onSelect}
            >
              {heading}
            </button>
          ))}
        <MarkdownContent className="bubble-markdown">{body}</MarkdownContent>
        {children}
        {variant === "speech" && createdAt !== undefined && (
          <small className="bubble-byline">
            {actor === "subagent" ? "Subagent" : "Aworkit"} · {createdAt}
          </small>
        )}
      </div>
    </article>
  );
}

function ActorAvatar({ actor }: { readonly actor: ChatActor }): React.JSX.Element {
  const label = actor === "subagent" ? "Subagent" : "Main model";
  return (
    <span
      aria-label={label}
      className={`actor-avatar actor-avatar-${actor}`}
      role="img"
      title={label}
    >
      {actor === "subagent" ? <RobotIcon /> : <span aria-hidden="true">AI</span>}
    </span>
  );
}

function RobotIcon(): React.JSX.Element {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24">
      <path d="M12 3v3M9.8 3h4.4M6.5 8.5h11a2 2 0 0 1 2 2v6.5a2 2 0 0 1-2 2h-11a2 2 0 0 1-2-2v-6.5a2 2 0 0 1 2-2Z" />
      <path d="M8 13h.01M16 13h.01M8.5 16h7M4.5 12H3M21 12h-1.5" />
    </svg>
  );
}
