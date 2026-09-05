import type { BuiltInToolConfiguration } from "../configuration";

/** Settings for HTTP input, inline previews, and the optional native fallback. */
export function WebExtractionSettingsEditor({ tool, onChange }: {
  readonly tool: BuiltInToolConfiguration;
  readonly onChange: (tool: BuiltInToolConfiguration) => void;
}): React.JSX.Element {
  const update = (key: string, value: number | boolean) => onChange({
    ...tool, configuration: { ...tool.configuration, [key]: value },
  });
  return <div className="settings-section-stack">
    <div className="settings-grid two-columns">
    <label className="settings-field" htmlFor={`${tool.id}-maximum-download-bytes`}>Maximum download bytes
      <input id={`${tool.id}-maximum-download-bytes`} type="number" min={1} max={8_388_608} step={1}
        title="Maximum decoded HTTP response bytes per page. Useful content is returned with an incomplete-source flag when this limit is reached."
        value={Number(tool.configuration.maximumDownloadBytes)}
        onChange={(event) => update("maximumDownloadBytes", Number(event.target.value))} />
    </label>
    <label className="settings-field" htmlFor={`${tool.id}-maximum-preview-bytes`}>Maximum preview bytes
      <input id={`${tool.id}-maximum-preview-bytes`} type="number" min={1} max={32_768} step={1}
        title="Maximum extracted UTF-8 bytes returned per page. The model output budget can reduce this further; saved-document continuation retrieves more."
        value={Number(tool.configuration.maximumExtractBytes)}
        onChange={(event) => update("maximumExtractBytes", Number(event.target.value))} />
    </label>
    </div>
    <label className="settings-inline-switches">
      <input id={`${tool.id}-render-when-needed`} type="checkbox"
        title="Allow one background WebView when a complete HTTP response needs JavaScript. It has a separate browsing profile and a 15-second deadline. A truncated download never triggers rendering."
        checked={tool.configuration.renderWhenNeeded !== false}
        onChange={(event) => update("renderWhenNeeded", event.target.checked)} />
      Render JavaScript when needed
    </label>
    <p className="provider-detail">Pages are downloaded and extracted locally first. Rendering loads page resources in a separate background browser with its own limits.</p>
  </div>;
}
