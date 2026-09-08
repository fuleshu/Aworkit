import { useMemo, useRef, useState } from "react";
import { prettyJson } from "./jsonPresentation";
import { rawJsonPages } from "./rawJsonPages";

/** Mounted only on the Raw tab: serialize once per value, lay out one bounded part. */
export function RawJsonView({ value }: { readonly value: unknown }): React.JSX.Element {
  const raw = useMemo(() => prettyJson(value, "The selected Run details are not serializable."), [value]);
  const pages = useMemo(() => rawJsonPages(raw), [raw]);
  const [requestedPage, setPage] = useState(0);
  const page = Math.min(requestedPage, pages.length - 1);
  const container = useRef<HTMLDivElement>(null);
  const selectPage = (next: number) => {
    setPage(Math.max(0, Math.min(pages.length - 1, Math.trunc(next))));
    const scroll = container.current?.closest(".run-details-content");
    if (scroll) scroll.scrollTop = 0;
  };
  return (
    <div ref={container}>
      <p className="run-details-raw-note">
        Exact redacted records for the currently selected scope.
      </p>
      <div className="run-details-json-toolbar">
        <button title="Copy the complete redacted JSON for this Run details scope" type="button"
          onClick={() => void navigator.clipboard?.writeText(raw)}>
          Copy JSON
        </button>
        {pages.length > 1 && (
          <nav className="run-details-json-pages" aria-label="Raw JSON parts">
            <button aria-label="Previous JSON part" title="Show the previous part of the JSON"
              disabled={page === 0} type="button" onClick={() => selectPage(page - 1)}>‹</button>
            <label>
              Part <input aria-label="JSON part" title="Jump to a part of the complete JSON"
                type="number" min={1} max={pages.length} value={page + 1}
                onChange={(event) => {
                  if (Number.isFinite(event.currentTarget.valueAsNumber)) selectPage(event.currentTarget.valueAsNumber - 1);
                }} /> of {pages.length.toLocaleString()}
            </label>
            <button aria-label="Next JSON part" title="Show the next part of the JSON"
              disabled={page === pages.length - 1} type="button" onClick={() => selectPage(page + 1)}>›</button>
          </nav>
        )}
      </div>
      <pre className="run-details-json">{pages[page]}</pre>
    </div>
  );
}
