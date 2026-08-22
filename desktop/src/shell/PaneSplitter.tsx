interface PaneSplitterProps {
  readonly value: number;
  readonly min: number;
  readonly max: number;
  readonly onChange: (value: number) => void;
  readonly label?: string;
  readonly direction?: 1 | -1;
  readonly className?: string;
}

/** Six-pixel pointer hit zone plus keyboard-adjustable separator semantics. */
export function PaneSplitter({
  value,
  min,
  max,
  onChange,
  label = "Resize navigation pane",
  direction = 1,
  className = "",
}: PaneSplitterProps): React.JSX.Element {
  const adjust = (delta: number) =>
    onChange(Math.min(max, Math.max(min, value + delta)));
  return (
    <div
      aria-label={label}
      aria-orientation="vertical"
      aria-valuemax={max}
      aria-valuemin={min}
      aria-valuenow={value}
      className={`pane-splitter ${className}`.trim()}
      role="separator"
      tabIndex={0}
      onKeyDown={(event) => {
        if (event.key === "ArrowLeft") adjust(-8 * direction);
        if (event.key === "ArrowRight") adjust(8 * direction);
        if (event.key === "Home") onChange(min);
        if (event.key === "End") onChange(max);
      }}
      onPointerDown={(event) => {
        const startX = event.clientX;
        const start = value;
        const move = (moveEvent: PointerEvent) =>
          onChange(
            Math.min(
              max,
              Math.max(min, start + direction * (moveEvent.clientX - startX)),
            ),
          );
        const up = () => {
          window.removeEventListener("pointermove", move);
          window.removeEventListener("pointerup", up);
        };
        window.addEventListener("pointermove", move);
        window.addEventListener("pointerup", up);
      }}
    />
  );
}
