import {
  releasedPayloadDigest,
  releasedPayloadEvidence,
  releasedRequestStatement,
  releasedSnapshotStatement,
  type ReleasedPayload,
} from "./releasedPayload";
import "./releasedPayload.css";

interface Props {
  readonly payload: ReleasedPayload;
  /** Context-specific placement, e.g. inside a model call block. */
  readonly className?: string;
}

/**
 * One stated omission for a released payload.
 *
 * The row, its usage and its timing stay exactly as they were; only the body
 * that retention removed is replaced by this statement, so the surface never
 * shows an empty panel, a null body or an error for a payload that is intact
 * in every other respect. The words follow the released field, not the surface
 * that is showing it.
 */
export function ReleasedPayloadNotice({
  payload,
  className,
}: Props): React.JSX.Element {
  const digest = releasedPayloadDigest(payload);
  const evidence = releasedPayloadEvidence(payload);
  const statement =
    payload.kind === "context_checkpoint"
      ? releasedSnapshotStatement(payload)
      : releasedRequestStatement(payload);
  return (
    <p
      className={`released-payload-notice ${className ?? ""}`.trim()}
      role="note"
      title={tooltip(digest, payload.reason)}
    >
      {statement}
      {evidence !== null && <small>{evidence}</small>}
    </p>
  );
}

/** Evidence stays available on demand instead of competing with the statement. */
function tooltip(digest: string | null, reason: string | null): string | undefined {
  const parts: string[] = [];
  if (digest !== null) parts.push(`Canonical digest before release: ${digest}`);
  if (reason !== null) parts.push(reason);
  return parts.length === 0 ? undefined : parts.join(" · ");
}
