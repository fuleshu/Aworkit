import { useMemo, useState } from "react";
import type { RuntimeEvent } from "./corePort";
import {
  formatRunDetailPrimitive,
  humanizeRunDetailLabel,
  projectRunDetails,
  rawRunDetailsJson,
  type RunDetailsLogEntry,
  type RunDetailsSection,
} from "./runDetails";
import type { ChatProjection, EvidenceRecord, TimelineItem } from "./types";

interface RunDetailsInspectorProps {
  readonly chat: ChatProjection;
  readonly events: readonly RuntimeEvent[];
  readonly items: readonly TimelineItem[];
  readonly records: readonly EvidenceRecord[];
  readonly selectedId: string | null;
  readonly onSelect: (id: string | null) => void;
  readonly onClose: () => void;
}

/** Contextual, human-readable Run inspector with raw data kept as an expert view. */
export function RunDetailsInspector({
  chat,
  events,
  items,
  records,
  selectedId,
  onSelect,
  onClose,
}: RunDetailsInspectorProps): React.JSX.Element {
  const [tab, setTab] = useState<"details" | "raw">("details");
  const view = useMemo(
    () => projectRunDetails({ chat, items, events, records, selectedId }),
    [chat, events, items, records, selectedId],
  );
  const raw = useMemo(() => rawRunDetailsJson(view), [view]);
  return (
    <aside className="run-details-inspector" aria-label="Run details">
      <header>
        <div>
          <p className="eyebrow">RUN DETAILS</p>
          <h2>{view.title}</h2>
        </div>
        <button
          aria-label="Close Run details"
          title="Close Run details"
          type="button"
          onClick={onClose}
        >
          ×
        </button>
      </header>
      <div className="inspector-tabs" role="tablist">
        <button
          aria-controls="run-details-panel"
          aria-selected={tab === "details"}
          role="tab"
          type="button"
          onClick={() => setTab("details")}
        >
          Details
        </button>
        <button
          aria-controls="run-details-panel"
          aria-selected={tab === "raw"}
          role="tab"
          type="button"
          onClick={() => setTab("raw")}
        >
          Raw JSON
        </button>
      </div>
      <nav className="run-details-breadcrumb" aria-label="Run details scope">
        {view.breadcrumbs.map((breadcrumb, index) => (
          <span key={breadcrumb.id ?? "entire-run"}>
            {index > 0 && <i aria-hidden="true">›</i>}
            <button
              aria-current={index === view.breadcrumbs.length - 1 ? "page" : undefined}
              title={`Show Run details for ${breadcrumb.label}`}
              type="button"
              onClick={() => onSelect(breadcrumb.id)}
            >
              {breadcrumb.label}
            </button>
          </span>
        ))}
      </nav>
      <div className="run-details-content" id="run-details-panel" role="tabpanel">
        {tab === "details" ? (
          <DetailsView sections={view.sections} summary={view.summary} onSelect={onSelect} />
        ) : (
          <>
            <p className="run-details-raw-note">
              Exact redacted records for the currently selected scope.
            </p>
            <pre className="run-details-json">{raw}</pre>
            <button
              title="Copy the redacted JSON for this Run details scope"
              type="button"
              onClick={() => void navigator.clipboard?.writeText(raw)}
            >
              Copy JSON
            </button>
          </>
        )}
      </div>
    </aside>
  );
}

function DetailsView({
  summary,
  sections,
  onSelect,
}: {
  readonly summary: ReturnType<typeof projectRunDetails>["summary"];
  readonly sections: readonly RunDetailsSection[];
  readonly onSelect: (id: string | null) => void;
}): React.JSX.Element {
  return (
    <>
      <DetailFields fields={summary} />
      {sections.map((section) => (
        <RunDetailsSectionView key={section.title} section={section} onSelect={onSelect} />
      ))}
    </>
  );
}

function RunDetailsSectionView({
  section,
  onSelect,
}: {
  readonly section: RunDetailsSection;
  readonly onSelect: (id: string | null) => void;
}): React.JSX.Element {
  return (
    <section className={`run-details-section run-details-section-kind-${section.kind}`}>
      <h3>{section.title}</h3>
      {section.kind === "fields" ? (
        <DetailFields fields={section.fields} />
      ) : section.kind === "text" ? (
        <p className="run-details-prose">{section.text}</p>
      ) : section.kind === "data" ? (
        <StructuredValue value={section.value} />
      ) : (
        <RunLog entries={section.entries} onSelect={onSelect} />
      )}
    </section>
  );
}

function DetailFields({
  fields,
}: {
  readonly fields: ReturnType<typeof projectRunDetails>["summary"];
}): React.JSX.Element {
  return (
    <dl className="run-details-fields">
      {fields.map((field) => (
        <div key={field.label}>
          <dt>{field.label}</dt>
          <dd>
            {field.status === undefined ? (
              field.value
            ) : (
              <span className={`status ${field.status}`}>{field.value}</span>
            )}
          </dd>
        </div>
      ))}
    </dl>
  );
}

function RunLog({
  entries,
  onSelect,
}: {
  readonly entries: readonly RunDetailsLogEntry[];
  readonly onSelect: (id: string | null) => void;
}): React.JSX.Element {
  if (entries.length === 0)
    return <p className="empty-state">No execution activity has been recorded.</p>;
  return (
    <ol className="run-details-log">
      {entries.map((entry) => (
        <li key={entry.id} style={{ paddingInlineStart: `${Math.min(entry.depth, 4) * 10}px` }}>
          <button
            title={`Show Run details for ${entry.title}`}
            type="button"
            onClick={() => onSelect(entry.id)}
          >
            <span>
              <strong>{entry.title}</strong>
              <small>{entry.kind} · {entry.time}</small>
            </span>
            <span className={`status ${entry.status}`}>{humanizeRunDetailLabel(entry.status)}</span>
          </button>
        </li>
      ))}
    </ol>
  );
}

function StructuredValue({ value }: { readonly value: unknown }): React.JSX.Element {
  if (Array.isArray(value)) {
    if (value.length === 0) return <p className="run-details-empty-value">None</p>;
    return (
      <ol className="run-details-value-list">
        {value.map((item, index) => (
          <li key={index}><StructuredValue value={item} /></li>
        ))}
      </ol>
    );
  }
  if (typeof value === "object" && value !== null) {
    const entries = Object.entries(value).filter(([, item]) => item !== undefined && item !== null);
    if (entries.length === 0) return <p className="run-details-empty-value">No values</p>;
    return (
      <dl className="run-details-structured-value">
        {entries.map(([key, item]) => (
          <div key={key}>
            <dt>{humanizeRunDetailLabel(key)}</dt>
            <dd><StructuredValue value={item} /></dd>
          </div>
        ))}
      </dl>
    );
  }
  return <p className="run-details-primitive">{formatRunDetailPrimitive(value)}</p>;
}
