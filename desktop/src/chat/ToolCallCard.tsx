import {
  Fragment,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { ImageAttachments } from "./ImageAttachments";
import { PathMenuTarget } from "./PathMenuTarget";
import { prettyJson } from "./jsonPresentation";
import type { ConversationCard } from "./conversation";
import type { TimelineItem } from "./types";
import {
  TOOL_INPUT_COLLAPSED_ROWS,
  TOOL_INPUT_MAX_ROWS,
  TOOL_OUTPUT_EXPANDED_ROWS,
  TOOL_OUTPUT_ROWS,
  fileToolPath,
  isToolBusy,
  toolArgumentSegments,
  toolHeaderLabel,
  toolInputIsMultiline,
  toolInputText,
  toolInputValue,
  toolOutputView,
  type ToolArgumentSegment,
  type ToolOutputView,
} from "./toolCallPresentation";
import "./toolCallCard.css";

/**
 * Native event asking the virtualized timeline to remeasure the row that owns
 * this card: expanding a field changes the row's height without new canonical
 * events, and the following rows must move with it.
 */
export const ACTIVITY_ROW_RESIZE_EVENT = "aworkit-activity-row-resize";

interface ToolCallCardProps {
  readonly card: ConversationCard;
  readonly item: TimelineItem;
  readonly selected: boolean;
  readonly onSelect: (id: string) => void;
}

/**
 * One tool call, presented for a reader rather than as a wire payload: the
 * capability id plus the real tool behind it, the arguments as a formatted
 * summary, and the output the tool itself produced — streaming while it runs,
 * previewed when it is an image, or reduced to a compact status otherwise.
 * The exact redacted JSON stays in Run details.
 */
export function ToolCallCard({
  card,
  item,
  selected,
  onSelect,
}: ToolCallCardProps): React.JSX.Element {
  const busy = isToolBusy(item.status);
  const segments = toolArgumentSegments(toolInputValue(item));
  const multilineInput = toolInputIsMultiline(segments);
  const output = toolOutputView(item);
  const [inputExpanded, setInputExpanded] = useState(false);
  const [outputExpanded, setOutputExpanded] = useState(false);
  const cardRef = useRef<HTMLElement>(null);
  const outputRef = useRef<HTMLPreElement>(null);
  const remeasured = useRef(false);
  const outputText =
    output === undefined || output.kind === "image" ? "" : output.text;

  // The field always shows the newest line of a streaming tool.
  useEffect(() => {
    const field = outputRef.current;
    if (field !== null) field.scrollTop = field.scrollHeight;
  }, [outputText]);

  // A field that grew or shrank outside the projection asks its row to remeasure.
  useEffect(() => {
    if (!remeasured.current) {
      remeasured.current = true;
      return;
    }
    cardRef.current?.dispatchEvent(
      new CustomEvent(ACTIVITY_ROW_RESIZE_EVENT, { bubbles: true }),
    );
  }, [inputExpanded, outputExpanded]);

  return (
    <article
      aria-busy={busy || undefined}
      aria-label={`${card.label}: ${item.title}`}
      className={`activity-card tool-card ${selected ? "selected" : ""}`}
      ref={cardRef}
    >
      <button
        className="activity-main"
        title={`Show Run details for ${item.title}`}
        type="button"
        onClick={() => onSelect(item.id)}
      >
        <span className="activity-icon">
          {busy ? "" : toolStatusIcon(item.status)}
        </span>
        <span>
          <strong>{toolHeaderLabel(item)}</strong>
        </span>
        <span className={`status ${item.status ?? ""}`}>
          {item.status ?? "tool"}
        </span>
      </button>

      {segments.length > 0 && (
        <div
          aria-label={`${item.title} input`}
          className="tool-io"
          role="group"
        >
          <div className="tool-io-head">
            <span className="tool-io-label">Input</span>
            {multilineInput && (
              <button
                aria-expanded={inputExpanded}
                aria-label={
                  inputExpanded
                    ? "Collapse tool input"
                    : "Expand tool input"
                }
                className="tool-io-expand"
                title={
                  inputExpanded
                    ? "Show the shortened arguments"
                    : "Show the complete formatted arguments"
                }
                type="button"
                onClick={() => setInputExpanded(!inputExpanded)}
              >
                {inputExpanded ? "Collapse" : "Expand"}
              </button>
            )}
          </div>
          <div
            className="tool-io-field"
            data-expanded={multilineInput ? inputExpanded : undefined}
            data-multiline={multilineInput || undefined}
            data-rows={
              multilineInput
                ? inputExpanded
                  ? TOOL_INPUT_MAX_ROWS
                  : TOOL_INPUT_COLLAPSED_ROWS
                : undefined
            }
            data-role="tool-input"
            style={
              multilineInput
                ? rowsStyle(
                    inputExpanded
                      ? TOOL_INPUT_MAX_ROWS
                      : TOOL_INPUT_COLLAPSED_ROWS,
                  )
                : undefined
            }
            title={toolInputText(segments)}
          >
            {segments.map((segment, index) => (
              <Fragment key={segment.key}>
                {index > 0 && (
                  <span className="tool-argument-separator"> · </span>
                )}
                <span className="tool-argument-label">{segment.label}: </span>
                <ToolArgumentValue item={item} segment={segment} />
              </Fragment>
            ))}
          </div>
        </div>
      )}

      {output !== undefined && (
        <ToolOutputField
          expanded={outputExpanded}
          item={item}
          output={output}
          outputRef={outputRef}
          onToggle={() => setOutputExpanded(!outputExpanded)}
        />
      )}

      {card.inspectable && (
        <details className="activity-raw">
          <summary>Inspect source record</summary>
          <pre>{safeJson(item.raw ?? item.metadata ?? item)}</pre>
        </details>
      )}
    </article>
  );
}

/** The tool's own result: text with a 4/10 row cap, an image, or a status. */
function ToolOutputField({
  expanded,
  item,
  onToggle,
  output,
  outputRef,
}: {
  readonly expanded: boolean;
  readonly item: TimelineItem;
  readonly onToggle: () => void;
  readonly output: ToolOutputView;
  readonly outputRef: React.RefObject<HTMLPreElement | null>;
}): React.JSX.Element {
  const rows = expanded ? TOOL_OUTPUT_EXPANDED_ROWS : TOOL_OUTPUT_ROWS;
  return (
    <div
      aria-label={`${item.title} output`}
      className="tool-io"
      role="group"
    >
      <div className="tool-io-head">
        <span className="tool-io-label">Output</span>
        {output.kind === "text" && (
          <button
            aria-expanded={expanded}
            aria-label={expanded ? "Collapse tool output" : "Expand tool output"}
            className="tool-io-expand"
            title={
              expanded
                ? "Show less of the tool output"
                : "Show more of the tool output"
            }
            type="button"
            onClick={onToggle}
          >
            {expanded ? "Collapse" : "Expand"}
          </button>
        )}
      </div>
      {output.kind === "image" ? (
        <ImageAttachments images={output.images} />
      ) : output.kind === "status" ? (
        <pre
          className="tool-io-field tool-io-status"
          data-role="tool-output"
        >
          {output.text}
        </pre>
      ) : (
        <pre
          className="tool-io-field"
          data-expanded={expanded}
          data-role="tool-output"
          data-rows={rows}
          ref={outputRef}
          style={rowsStyle(rows)}
        >
          {output.text}
        </pre>
      )}
    </div>
  );
}

/**
 * One argument value. A path a file tool acted on stays the element a user can
 * right-click to open, edit or reveal, exactly as the conversation shows it.
 */
function ToolArgumentValue({
  item,
  segment,
}: {
  readonly item: TimelineItem;
  readonly segment: ToolArgumentSegment;
}): React.JSX.Element {
  if (segment.key === "path" && fileToolPath(item) === segment.text) {
    return (
      <PathMenuTarget path={segment.text}>
        <span
          className="tool-argument-path"
          title={`Workspace path: ${segment.text}. Right-click to open, edit or reveal it.`}
        >
          {segment.text}
        </span>
      </PathMenuTarget>
    );
  }
  return <Fragment>{segment.text}</Fragment>;
}

/** Row caps are multiplied by the live code line height in the stylesheet. */
function rowsStyle(rows: number): CSSProperties {
  return { "--aw-tool-rows": rows } as CSSProperties;
}

function toolStatusIcon(status: string | undefined): string {
  if (status === "failed" || status === "cancelled") return "×";
  return ">_";
}

function safeJson(value: unknown): string {
  return prettyJson(value, "Source record is not serializable.");
}
