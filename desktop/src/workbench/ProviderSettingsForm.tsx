import type { ProviderTestResult } from "./corePort";
import type { ProviderDraft } from "./settings";

interface ProviderStatus {
  readonly state: ProviderDraft["state"];
  readonly detail: string | null;
}

/** Editable provider form; secret input remains local and write-only. */
export function ProviderSettingsForm({
  draft,
  status,
  validationError,
  testing,
  testResult,
  onChange,
  onTest,
}: {
  readonly draft: ProviderDraft;
  readonly status: ProviderStatus;
  readonly validationError: string | null;
  readonly testing: boolean;
  readonly testResult: ProviderTestResult | null;
  readonly onChange: (patch: Partial<ProviderDraft>) => void;
  readonly onTest: () => void;
}): React.JSX.Element {
  const canTest =
    !testing &&
    validationError === null &&
    draft.baseUrl.trim() !== "" &&
    draft.model.trim() !== "";
  return (
    <div className="provider-settings">
      <p className="section-intro">
        Configure the OpenAI-compatible endpoint used by the bundled Simple
        Chat workflow. A saved provider is not considered ready until a real
        connection test succeeds.
      </p>
      <section className="provider-card" aria-labelledby="provider-heading">
        <div className="provider-card-heading">
          <div>
            <h3 id="provider-heading">OpenAI-compatible provider</h3>
            <p>One endpoint and one model are supported in this rescue slice.</p>
          </div>
          <span className={`status ${status.state}`}>{status.state}</span>
        </div>
        <div className="provider-fields">
          <label htmlFor="provider-base-url">
            Base URL
            <input
              autoCapitalize="none"
              autoComplete="url"
              id="provider-base-url"
              placeholder="https://api.openai.com/v1"
              spellCheck={false}
              title="HTTP or HTTPS base URL of an OpenAI-compatible API, usually ending in /v1"
              type="url"
              value={draft.baseUrl}
              onChange={(event) => onChange({ baseUrl: event.target.value })}
            />
          </label>
          <label htmlFor="provider-model">
            Model ID
            <input
              autoCapitalize="none"
              autoComplete="off"
              id="provider-model"
              placeholder="gpt-4.1-mini"
              spellCheck={false}
              title="Exact model identifier sent to the provider"
              type="text"
              value={draft.model}
              onChange={(event) => onChange({ model: event.target.value })}
            />
          </label>
          <label htmlFor="provider-api-key">
            API key <span>optional</span>
            <input
              autoCapitalize="none"
              autoComplete="new-password"
              disabled={draft.credentialAction === "clear"}
              id="provider-api-key"
              placeholder={
                draft.credentialConfigured
                  ? "Stored key unchanged"
                  : "No key configured"
              }
              spellCheck={false}
              title="Enter a new API key to replace the securely stored key; leave blank to keep it unchanged"
              type="password"
              value={draft.apiKey}
              onChange={(event) => {
                const apiKey = event.target.value;
                onChange({
                  apiKey,
                  credentialAction: apiKey === "" ? "keep" : "replace",
                });
              }}
            />
          </label>
          <label
            className="switch-label provider-clear"
            htmlFor="provider-clear-key"
          >
            <input
              checked={draft.credentialAction === "clear"}
              id="provider-clear-key"
              title="Remove any securely stored API key when settings are saved"
              type="checkbox"
              onChange={(event) =>
                onChange({
                  apiKey: "",
                  credentialAction: event.target.checked ? "clear" : "keep",
                })
              }
            />
            Clear saved API key
          </label>
        </div>
        <div className="provider-security-note">
          <span>
            Credential: {draft.credentialConfigured ? "stored securely" : "not stored"}
          </span>
          <small>
            The desktop core never returns the saved key to this screen.
          </small>
        </div>
        <div className="provider-actions">
          <button
            disabled={!canTest}
            title={
              canTest
                ? "Contact the configured endpoint and verify that the model is available"
                : validationError ??
                  "Enter a base URL and model ID before testing"
            }
            type="button"
            onClick={onTest}
          >
            {testing ? "Testing…" : "Test connection"}
          </button>
          <p className={`provider-detail ${status.state}`} role="status">
            {status.detail ?? "Provider status is unavailable."}
            {testResult?.model !== null && testResult?.model !== undefined
              ? ` Reported model: ${testResult.model}.`
              : ""}
          </p>
        </div>
        {validationError !== null && (
          <p className="field-error" role="alert">
            {validationError}
          </p>
        )}
      </section>
    </div>
  );
}

export function providerValidationError(provider: ProviderDraft): string | null {
  const baseUrl = provider.baseUrl.trim();
  const model = provider.model.trim();
  if (baseUrl === "" && model !== "") return "Enter a base URL for this model.";
  if (baseUrl !== "" && model === "") return "Enter the provider model ID.";
  if (baseUrl !== "") {
    try {
      const url = new URL(baseUrl);
      if (url.protocol !== "http:" && url.protocol !== "https:")
        return "The base URL must use HTTP or HTTPS.";
    } catch {
      return "Enter a valid provider base URL.";
    }
  }
  if (
    provider.credentialAction === "replace" &&
    provider.apiKey.trim() === ""
  )
    return "Enter a non-empty replacement API key.";
  return null;
}

export function providerFingerprint(provider: ProviderDraft): string {
  return JSON.stringify({
    baseUrl: provider.baseUrl.trim(),
    model: provider.model.trim(),
    credentialAction: provider.credentialAction,
    apiKey: provider.credentialAction === "replace" ? provider.apiKey : null,
  });
}
