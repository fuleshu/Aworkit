import { projectAppearancePreference } from "../appearance";
import type { AppearanceConfiguration } from "../configuration";

const FONT_SCALES = [0.85, 1, 1.15, 1.3, 1.5] as const;

export function AppearanceSection({
  value,
  onChange,
  onReset,
}: {
  readonly value: AppearanceConfiguration;
  readonly onChange: (value: AppearanceConfiguration) => void;
  readonly onReset: () => void;
}): React.JSX.Element {
  const update = (next: AppearanceConfiguration) => {
    onChange(next);
    projectAppearancePreference(next.mode, next.fontScale);
  };
  return (
    <div className="settings-section-stack">
      <p className="section-intro">
        Preview color and text size immediately. Save stores this preference;
        Discard restores the last committed appearance.
      </p>
      <fieldset className="appearance-options">
        <legend>Color mode</legend>
        {(["system", "light", "dark"] as const).map((mode) => (
          <label key={mode}>
            <input
              checked={value.mode === mode}
              name="appearance-v2-mode"
              title={`Preview ${mode} color mode`}
              type="radio"
              onChange={() => update({ ...value, mode })}
            />
            <span className={`theme-preview ${mode}`} />
            <strong>{mode.replace(/^./u, (letter) => letter.toUpperCase())}</strong>
            <small>
              {mode === "system"
                ? "Follow live operating-system appearance"
                : `Always use ${mode} appearance`}
            </small>
          </label>
        ))}
      </fieldset>
      <label className="settings-field" htmlFor="appearance-font-scale">
        Text size
        <select
          id="appearance-font-scale"
          title="Preview the application text scale; 100% is the default"
          value={String(value.fontScale)}
          onChange={(event) =>
            update({ ...value, fontScale: Number(event.target.value) })
          }
        >
          {FONT_SCALES.map((scale) => (
            <option key={scale} value={scale}>
              {Math.round(scale * 100)}%
            </option>
          ))}
        </select>
      </label>
      <div className="section-actions">
        <button
          title="Reset appearance to System mode and 100% text size"
          type="button"
          onClick={onReset}
        >
          Reset default
        </button>
      </div>
    </div>
  );
}
