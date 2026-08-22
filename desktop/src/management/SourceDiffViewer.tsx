import { useVirtualizer } from "@tanstack/react-virtual";
import { useMemo, useRef, useState } from "react";
import type { RepairDiffLineV1, RepairSourceDiffV1 } from "./types";

/** Selectable, virtualized source diff with accessible table semantics. */
export function SourceDiffViewer({
  files,
}: {
  readonly files: readonly RepairSourceDiffV1[];
}): React.JSX.Element {
  const [selectedId, setSelectedId] = useState(files[0]?.id ?? "");
  const selected = files.find(({ id }) => id === selectedId) ?? files[0];
  if (selected === undefined) return <p>No source changes.</p>;
  return (
    <div className="source-diff-viewer">
      <div aria-label="Changed source files" className="diff-file-tabs" role="tablist">
        {files.map((file) => (
          <button
            aria-controls="selected-source-diff"
            aria-selected={file.id === selected.id}
            key={file.id}
            role="tab"
            title={`Show the complete diff for ${file.path}`}
            type="button"
            onClick={() => setSelectedId(file.id)}
          >
            {file.path} · {file.linesChanged} lines changed
          </button>
        ))}
      </div>
      <VirtualDiff file={selected} />
    </div>
  );
}

/** Virtualized rows keep large diffs bounded while retaining table semantics. */
function VirtualDiff({ file }: { readonly file: RepairSourceDiffV1 }): React.JSX.Element {
  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: file.lines.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 28,
    overscan: 8,
    initialRect: { width: 800, height: 190 },
  });
  const virtualRows = virtualizer.getVirtualItems();
  const visibleRows = useMemo(
    () =>
      virtualRows.length === 0
        ? file.lines.map((_, index) => ({ index, start: index * 28, size: 28 }))
        : virtualRows,
    [file.lines, virtualRows],
  );
  return (
    <div
      aria-colcount={4}
      aria-label={`Complete source diff for ${file.path}`}
      aria-rowcount={file.lines.length}
      className="virtual-diff"
      id="selected-source-diff"
      ref={scrollRef}
      role="table"
      tabIndex={0}
    >
      <div
        role="rowgroup"
        style={{ height: Math.max(virtualizer.getTotalSize(), file.lines.length * 28) }}
      >
        {visibleRows.map((row) => (
          <DiffRow
            key={file.lines[row.index].id}
            line={file.lines[row.index]}
            rowIndex={row.index}
            start={row.start}
          />
        ))}
      </div>
    </div>
  );
}

function DiffRow({
  line,
  rowIndex,
  start,
}: {
  readonly line: RepairDiffLineV1;
  readonly rowIndex: number;
  readonly start: number;
}): React.JSX.Element {
  const marker = line.kind === "added" ? "+" : line.kind === "removed" ? "−" : " ";
  return (
    <div
      aria-label={`${line.kind} source line`}
      aria-rowindex={rowIndex + 1}
      className={`diff-row ${line.kind}`}
      role="row"
      style={{ transform: `translateY(${start}px)` }}
    >
      <span aria-label={line.kind} className="diff-marker" role="cell">
        {marker}
      </span>
      <span aria-label="Old line" role="cell">
        {line.oldLine ?? ""}
      </span>
      <span aria-label="New line" role="cell">
        {line.newLine ?? ""}
      </span>
      <code role="cell">{line.content}</code>
    </div>
  );
}
