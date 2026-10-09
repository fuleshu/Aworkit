import { ActorBubble } from "./ActorBubble";
import { prettyJson } from "./jsonPresentation";
import { releasedModelCallInput } from "./releasedPayload";
import { isSelectionClick } from "./selectionClick";
import {
  isModelCallSpan,
  modelCallAssistantOutput,
  modelCallReasoning,
} from "./modelCallChannels";
import type { TimelineActor, TimelineItem } from "./types";

export { isModelCallSpan, modelCallAssistantOutput, modelCallReasoning };

interface ModelCallBlockProps {
  readonly item: TimelineItem;
  readonly selected: boolean;
  /** Hides the provider-supplied reasoning while keeping the model's answer. */
  readonly hideThinking?: boolean;
  readonly onSelect: (id: string) => void;
}

interface WorkflowNodeContext {
  readonly name: string;
  readonly type: string;
}

/**
 * Presents one provider invocation as a single selectable lifecycle while
 * retaining the established actor-owned thought and speech bubble language.
 */
export function ModelCallBlock({
  item,
  selected,
  hideThinking = false,
  onSelect,
}: ModelCallBlockProps): React.JSX.Element {
  const metadata = record(item.metadata);
  const node = workflowNodeContext(item);
  const reasoning = modelCallReasoning(item);
  const actor: TimelineActor = item.actor ?? "model";
  const busy = isBusy(item.status);
  // Early v1 producers could place a synchronous live chunk directly in the
  // span body. Preserve that transient path as speech until a typed channel
  // arrives; terminal lifecycle summaries must never become model speech.
  const assistantOutput =
    modelCallAssistantOutput(item) ||
    (busy && reasoning.length === 0 ? (item.body ?? "") : "");
  const thinkingBusy = busy && assistantOutput.length === 0;
  const hasInput = item.input !== undefined || metadata.hasInput === true;
  const hasOutput = item.output !== undefined || metadata.hasOutput === true;
  const input = item.input !== undefined ? item.input : metadata.input;
  const output = item.output !== undefined ? item.output : metadata.output;
  // A released request body keeps `hasInput`, because the call really carried a
  // request, so the omission is stated where the body would have been rendered.
  // A payload that still carries its body renders exactly as it did before.
  const releasedRequest =
    input === undefined ? releasedModelCallInput(metadata) : undefined;

  return (
    <section
      aria-busy={busy || undefined}
      aria-label={`Model call: ${node.name}`}
      className={`model-call-block ${selected ? "selected" : ""}`}
      role="group"
      tabIndex={0}
      title={`Show Run details for ${node.name}`}
      onClick={(event) => {
        if (isSelectionClick(event)) onSelect(item.id);
      }}
      onKeyDown={(event) => {
        if (
          event.currentTarget === event.target &&
          (event.key === "Enter" || event.key === " ")
        ) {
          event.preventDefault();
          onSelect(item.id);
        }
      }}
    >
      <header className="model-call-heading">
        <span className="model-call-status-mark" aria-hidden="true">
          {busy ? "" : statusIcon(item.status)}
        </span>
        <span className="model-call-heading-copy">
          <strong>{node.name}</strong>
          <small>
            {nodeTypeLabel(node.type)} · {item.title}
          </small>
        </span>
        <span className={`status ${item.status ?? ""}`}>
          {item.status ?? "running"}
        </span>
      </header>

      {releasedRequest === undefined && hasInput && (
        <ModelCallData label="Input" value={input} />
      )}

      <div className="model-call-stream">
        {!hideThinking && reasoning.length > 0 && (
          <ActorBubble
            actor={actor}
            ariaLabel={`Thinking: ${item.title}`}
            body={reasoning}
            busy={thinkingBusy}
            reasoningLabel={reasoningLabel(item.reasoningCategory)}
            status={thinkingBusy ? "running" : "completed"}
            title="Thinking"
            variant="thinking"
          />
        )}
        {assistantOutput.length > 0 && (
          <ActorBubble
            actor={actor}
            ariaLabel={`Model output: ${item.title}`}
            body={assistantOutput}
            busy={busy}
            createdAt={busy ? undefined : item.createdAt}
            variant="speech"
          />
        )}
      </div>

      {!busy && hasOutput && (
        <ModelCallData label="Output" value={output} />
      )}
    </section>
  );
}

function ModelCallData({
  label,
  value,
}: {
  readonly label: "Input" | "Output";
  readonly value: unknown;
}): React.JSX.Element {
  return (
    <details className="model-call-data">
      <summary>{label}</summary>
      <pre aria-label={`${label} JSON`}>
        {prettyJson(value, "The model data is not serializable.")}
      </pre>
    </details>
  );
}

function workflowNodeContext(item: TimelineItem): WorkflowNodeContext {
  const metadata = record(item.metadata);
  const node = record(metadata.workflowNode);
  return {
    name: text(node.name) || text(metadata.label) || item.title,
    type: text(node.type) || text(metadata.nodeType) || "model_call",
  };
}

function reasoningLabel(
  category: TimelineItem["reasoningCategory"],
): string | undefined {
  if (category === "summary") return "Provider reasoning summary";
  if (category === "progress") return "Provider progress";
  if (category === "source_provided") return "Provider-supplied reasoning";
  return undefined;
}

function nodeTypeLabel(value: string): string {
  const normalized = value.replaceAll("_", " ").trim();
  if (normalized.length === 0) return "Model call node";
  return `${normalized.charAt(0).toUpperCase()}${normalized.slice(1)} node`;
}

function statusIcon(status: string | undefined): string {
  if (status === "failed") return "×";
  if (status === "cancelled") return "×";
  if (status === "completed" || status === "succeeded") return "✓";
  return "◇";
}

function isBusy(status: string | undefined): boolean {
  return status === "running" || status === "started" || status === "queued";
}

function record(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}
