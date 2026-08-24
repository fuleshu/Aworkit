import { useEffect, useMemo, useState } from "react";
import {
  inspectEvidence,
  queryEvidence,
  redactedEvidenceJson,
} from "./evidence";
import type { EvidenceRecord } from "./types";

interface EvidenceInspectorProps {
  readonly records: readonly EvidenceRecord[];
  readonly selectedId: string | null;
  readonly onClose: () => void;
}

/** Dockable expert inspector that never fills unavailable evidence with inferred content. */
export function EvidenceInspector({
  records,
  selectedId,
  onClose,
}: EvidenceInspectorProps): React.JSX.Element {
  const [filter, setFilter] = useState<EvidenceRecord["category"] | "all">(
    "all",
  );
  const [offset, setOffset] = useState(0);
  const [tab, setTab] = useState<"details" | "raw">("details");
  const [activeId, setActiveId] = useState<string | null>(selectedId);
  const page = useMemo(
    () =>
      queryEvidence(records, {
        filter: filter === "all" ? undefined : filter,
        offset,
        limit: 10,
      }),
    [filter, offset, records],
  );
  useEffect(() => {
    if (selectedId === null) return;
    const index = records.findIndex(({ id }) => id === selectedId);
    if (index < 0) return;
    setFilter("all");
    setOffset(Math.floor(index / 10) * 10);
    setActiveId(selectedId);
  }, [records, selectedId]);
  const selected =
    page.items.find((record) => record.id === activeId) ?? page.items[0];
  return (
    <aside className="evidence-inspector" aria-label="Evidence inspector">
      <header>
        <div>
          <p className="eyebrow">EVIDENCE</p>
          <h2>{selected?.label ?? "No selection"}</h2>
        </div>
        <button
          aria-label="Close evidence inspector"
          title="Close evidence inspector"
          type="button"
          onClick={onClose}
        >
          ×
        </button>
      </header>
      <div className="inspector-tabs" role="tablist">
        <button
          aria-controls="evidence-panel"
          aria-selected={tab === "details"}
          role="tab"
          type="button"
          onClick={() => setTab("details")}
        >
          Details
        </button>
        <button
          aria-controls="evidence-panel"
          aria-selected={tab === "raw"}
          role="tab"
          type="button"
          onClick={() => setTab("raw")}
        >
          Raw JSON
        </button>
      </div>
      <label className="inspector-filter">
        Filter
        <select
          title="Filter evidence by category"
          value={filter}
          onChange={(event) => {
            setFilter(event.target.value as typeof filter);
            setOffset(0);
            setActiveId(null);
          }}
        >
          <option value="all">All evidence</option>
          <option value="provenance">Provenance</option>
          <option value="usage">Usage and cost</option>
          <option value="routing">Routing</option>
          <option value="approval">Approvals</option>
          <option value="artifact">Artifacts</option>
          <option value="retry">Retries</option>
          <option value="opacity">Source opacity</option>
          <option value="retention">Retention</option>
          <option value="debug">Debug capture</option>
          <option value="unknown">Unknown category</option>
        </select>
      </label>
      <div className="evidence-detail" id="evidence-panel" role="tabpanel">
        {page.items.length > 1 && (
          <div className="evidence-record-list" aria-label="Evidence records">
            {page.items.map((record) => (
              <button
                aria-pressed={record.id === selected?.id}
                key={record.id}
                title={`Inspect ${record.label}`}
                type="button"
                onClick={() => setActiveId(record.id)}
              >
                <span>{record.label}</span>
                <span className={`status ${record.state}`}>{record.state}</span>
              </button>
            ))}
          </div>
        )}
        {selected === undefined ? (
          <p className="empty-state">
            No evidence is available for this filter.
          </p>
        ) : tab === "details" ? (
          <>
            <dl>
              <div>
                <dt>State</dt>
                <dd>
                  <span className={`status ${selected.state}`}>
                    {selected.state}
                  </span>
                </dd>
              </div>
              <div>
                <dt>Category</dt>
                <dd>{selected.category}</dd>
              </div>
            </dl>
            <pre>{inspectEvidence(selected)}</pre>
          </>
        ) : (
          <pre>{redactedEvidenceJson(selected)}</pre>
        )}
        {selected !== undefined && (
          <button
            title="Copy the safely redacted evidence value"
            type="button"
            onClick={() =>
              void navigator.clipboard?.writeText(
                tab === "raw"
                  ? redactedEvidenceJson(selected)
                  : inspectEvidence(selected),
              )
            }
          >
            Copy redacted value
          </button>
        )}
      </div>
      <footer>
        <span>{page.total} records</span>
        <button
          disabled={offset === 0}
          title="Show previous evidence page"
          type="button"
          onClick={() => setOffset(Math.max(0, offset - 10))}
        >
          Previous
        </button>
        <button
          disabled={offset + 10 >= page.total}
          title="Show next evidence page"
          type="button"
          onClick={() => setOffset(offset + 10)}
        >
          Next
        </button>
      </footer>
    </aside>
  );
}
