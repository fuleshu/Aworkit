import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import type {
  ConnectionConfiguration,
  CredentialMetadataConfiguration,
} from "../configuration";

export type CredentialBinding = {
  readonly name: string;
  readonly credentialRef: string;
  readonly field: string;
};

type JsonValidationReporter = (id: string, message: string | null) => void;

const JsonValidationContext = createContext<JsonValidationReporter>(() => {});

/** Aggregates invalid JSON editors so the parent Save action can fail closed. */
export function SettingsFieldValidationBoundary({
  children,
  onChange,
}: {
  readonly children: ReactNode;
  readonly onChange: (errors: Readonly<Record<string, string>>) => void;
}): React.JSX.Element {
  const [errors, setErrors] = useState<Readonly<Record<string, string>>>({});
  const report = useCallback<JsonValidationReporter>((id, message) => {
    setErrors((current) => {
      if (message === null) {
        if (!(id in current)) return current;
        const next = { ...current };
        delete next[id];
        return next;
      }
      return current[id] === message ? current : { ...current, [id]: message };
    });
  }, []);

  useEffect(() => onChange(errors), [errors, onChange]);

  return (
    <JsonValidationContext.Provider value={report}>
      {children}
    </JsonValidationContext.Provider>
  );
}

/** Edits an arbitrary, non-secret JSON object without discarding invalid text. */
export function JsonObjectField({
  id,
  label,
  title,
  value,
  onChange,
  onErrorChange,
}: {
  readonly id: string;
  readonly label: string;
  readonly title: string;
  readonly value: Readonly<Record<string, unknown>>;
  readonly onChange: (value: Record<string, unknown>) => void;
  readonly onErrorChange?: (message: string | null) => void;
}): React.JSX.Element {
  const reportValidation = useContext(JsonValidationContext);
  const canonical = useMemo(() => JSON.stringify(value, null, 2), [value]);
  const [source, setSource] = useState(canonical);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setSource(canonical);
    setError(null);
    reportValidation(id, null);
    onErrorChange?.(null);
    return () => reportValidation(id, null);
  }, [canonical, id, reportValidation]);

  const update = (next: string) => {
    setSource(next);
    try {
      const parsed: unknown = JSON.parse(next);
      if (
        parsed === null ||
        Array.isArray(parsed) ||
        typeof parsed !== "object"
      ) {
        throw new Error("Configuration must be a JSON object.");
      }
      setError(null);
      reportValidation(id, null);
      onErrorChange?.(null);
      onChange(parsed as Record<string, unknown>);
    } catch (failure) {
      const message =
        failure instanceof Error ? failure.message : "Invalid JSON object.";
      setError(message);
      reportValidation(id, message);
      onErrorChange?.(message);
    }
  };

  return (
    <label className="settings-field settings-json-field" htmlFor={id}>
      {label}
      <textarea
        aria-describedby={error === null ? undefined : `${id}-error`}
        aria-invalid={error === null ? undefined : true}
        id={id}
        spellCheck={false}
        title={title}
        value={source}
        onChange={(event) => update(event.target.value)}
      />
      {error !== null && (
        <small className="field-error" id={`${id}-error`} role="alert">
          {error}
        </small>
      )}
    </label>
  );
}

/** Edits named secret-field bindings while keeping every secret value opaque. */
export function CredentialBindingsEditor({
  id,
  label,
  bindings,
  credentials,
  onChange,
}: {
  readonly id: string;
  readonly label: string;
  readonly bindings: readonly CredentialBinding[];
  readonly credentials: readonly CredentialMetadataConfiguration[];
  readonly onChange: (bindings: readonly CredentialBinding[]) => void;
}): React.JSX.Element {
  const add = () => {
    const credential = credentials[0];
    if (credential === undefined) return;
    onChange([
      ...bindings,
      {
        name: `binding_${bindings.length + 1}`,
        credentialRef: credential.credentialRef,
        field: credential.fieldNames[0] ?? "value",
      },
    ]);
  };
  return (
    <fieldset className="settings-bindings">
      <legend>{label}</legend>
      {bindings.length === 0 ? (
        <p className="settings-empty">No credential fields are exposed.</p>
      ) : (
        <div className="settings-binding-list">
          {bindings.map((binding, index) => {
            const credential = credentials.find(
              ({ credentialRef }) => credentialRef === binding.credentialRef,
            );
            return (
              <div className="settings-binding-row" key={`${id}-${index}`}>
                <label className="settings-field" htmlFor={`${id}-${index}-name`}>
                  Injection name
                  <input
                    id={`${id}-${index}-name`}
                    title="Header, environment variable, or integration field name that receives the leased secret"
                    type="text"
                    value={binding.name}
                    onChange={(event) =>
                      onChange(
                        replaceAt(bindings, index, {
                          ...binding,
                          name: event.target.value,
                        }),
                      )
                    }
                  />
                </label>
                <label
                  className="settings-field"
                  htmlFor={`${id}-${index}-credential`}
                >
                  Credential
                  <select
                    id={`${id}-${index}-credential`}
                    title="Opaque operating-system credential-store record to lease for this integration"
                    value={binding.credentialRef}
                    onChange={(event) => {
                      const selected = credentials.find(
                        ({ credentialRef }) =>
                          credentialRef === event.target.value,
                      );
                      onChange(
                        replaceAt(bindings, index, {
                          ...binding,
                          credentialRef: event.target.value,
                          field: selected?.fieldNames[0] ?? "",
                        }),
                      );
                    }}
                  >
                    {credentials.map((item) => (
                      <option key={item.credentialRef} value={item.credentialRef}>
                        {item.label}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="settings-field" htmlFor={`${id}-${index}-field`}>
                  Secret field
                  <select
                    id={`${id}-${index}-field`}
                    title="Named field leased from the selected credential; its value is never returned to this screen"
                    value={binding.field}
                    onChange={(event) =>
                      onChange(
                        replaceAt(bindings, index, {
                          ...binding,
                          field: event.target.value,
                        }),
                      )
                    }
                  >
                    {(credential?.fieldNames ?? []).map((field) => (
                      <option key={field} value={field}>
                        {field}
                      </option>
                    ))}
                  </select>
                </label>
                <button
                  aria-label={`Remove ${binding.name || "credential binding"}`}
                  className="danger-action"
                  title="Remove this credential-field binding without deleting the credential"
                  type="button"
                  onClick={() =>
                    onChange(bindings.filter((_, itemIndex) => itemIndex !== index))
                  }
                >
                  Remove
                </button>
              </div>
            );
          })}
        </div>
      )}
      <button
        disabled={credentials.length === 0}
        title={
          credentials.length === 0
            ? "Create a credential before adding a secret-field binding"
            : "Add an opaque credential-field binding"
        }
        type="button"
        onClick={add}
      >
        Add credential binding
      </button>
    </fieldset>
  );
}

/** Shared HTTP/stdio transport editor for MCP and external-agent adapters. */
export function ConnectionEditor({
  id,
  value,
  credentials,
  allowedTransports = ["stdio", "http"],
  showWorkingDirectory = true,
  onPickCommand,
  onChange,
}: {
  readonly id: string;
  readonly value: ConnectionConfiguration;
  readonly credentials: readonly CredentialMetadataConfiguration[];
  readonly allowedTransports?: readonly ("http" | "stdio")[];
  /** MCP processes inherit Aworkit's directory; external agents may opt in. */
  readonly showWorkingDirectory?: boolean;
  /** Opens a native executable picker when the containing screen provides one. */
  readonly onPickCommand?: () => Promise<string | null>;
  readonly onChange: (value: ConnectionConfiguration) => void;
}): React.JSX.Element {
  const selectTransport = (transport: "http" | "stdio") => {
    if (transport === value.transport) return;
    onChange(
      transport === "http"
        ? { transport: "http", url: "http://127.0.0.1:3000/mcp", headers: [] }
        : { transport: "stdio", command: "", args: [], cwd: null, env: [] },
    );
  };
  return (
    <div className="settings-section-stack settings-transport">
      <label className="settings-field" htmlFor={`${id}-transport`}>
        Transport
        <select
          id={`${id}-transport`}
          title={
            allowedTransports.length === 1
              ? "This installed adapter supports only a supervised standard-I/O process"
              : "Use Streamable HTTP for a network endpoint or standard I/O for a supervised local process"
          }
          value={value.transport}
          onChange={(event) =>
            selectTransport(event.target.value as "http" | "stdio")
          }
        >
          {allowedTransports.includes("stdio") && (
            <option value="stdio">Standard I/O process</option>
          )}
          {allowedTransports.includes("http") && (
            <option value="http">Streamable HTTP</option>
          )}
          {!allowedTransports.includes(value.transport) && (
            <option disabled value={value.transport}>
              {value.transport === "http"
                ? "Streamable HTTP (adapter not installed)"
                : "Standard I/O process (adapter not installed)"}
            </option>
          )}
        </select>
      </label>
      {!allowedTransports.includes(value.transport) && (
        <p className="field-warning" role="alert">
          This saved transport is not supported by the selected adapter. Choose
          an installed transport before probing or saving it as ready.
        </p>
      )}
      {value.transport === "http" ? (
        <>
          <label className="settings-field" htmlFor={`${id}-url`}>
            Server URL
            <input
              id={`${id}-url`}
              spellCheck={false}
              title="Absolute HTTP(S) URL without embedded credentials, query, or fragment; secrets must use header bindings"
              type="url"
              value={value.url}
              onChange={(event) => onChange({ ...value, url: event.target.value })}
            />
          </label>
          <CredentialBindingsEditor
            id={`${id}-headers`}
            label="Secret-backed HTTP headers"
            bindings={value.headers}
            credentials={credentials}
            onChange={(headers) => onChange({ ...value, headers: [...headers] })}
          />
        </>
      ) : (
        <>
          <div className="settings-grid two-columns">
            <div className="settings-field">
              <label htmlFor={`${id}-command`}>Command</label>
              <div className="settings-command-picker">
                <input
                  id={`${id}-command`}
                  spellCheck={false}
                  title="Exact executable to launch. Windows paths may contain spaces; shell quotes are optional."
                  type="text"
                  value={value.command}
                  onChange={(event) =>
                    onChange({ ...value, command: event.target.value })
                  }
                />
                {onPickCommand !== undefined && (
                  <button
                    title="Choose the MCP executable with the native file picker"
                    type="button"
                    onClick={() => {
                      void onPickCommand().then((command) => {
                        if (command === null) return;
                        onChange({ ...value, command, cwd: null });
                      });
                    }}
                  >
                    Browse…
                  </button>
                )}
              </div>
            </div>
            {showWorkingDirectory && (
              <label className="settings-field" htmlFor={`${id}-cwd`}>
                Working directory
                <input
                  id={`${id}-cwd`}
                  spellCheck={false}
                  title="Optional working directory; this is not a security restriction"
                  type="text"
                  value={value.cwd ?? ""}
                  onChange={(event) =>
                    onChange({ ...value, cwd: event.target.value || null })
                  }
                />
              </label>
            )}
          </div>
          <label className="settings-field" htmlFor={`${id}-args`}>
            Arguments
            <textarea
              id={`${id}-args`}
              spellCheck={false}
              title="One public command argument per line; authentication values are rejected and must use secret-backed environment bindings"
              value={value.args.join("\n")}
              onChange={(event) =>
                onChange({
                  ...value,
                  args: event.target.value === "" ? [] : event.target.value.split("\n"),
                })
              }
            />
          </label>
          <CredentialBindingsEditor
            id={`${id}-env`}
            label="Secret-backed environment variables"
            bindings={value.env}
            credentials={credentials}
            onChange={(env) => onChange({ ...value, env: [...env] })}
          />
        </>
      )}
    </div>
  );
}

function replaceAt<T>(values: readonly T[], index: number, value: T): T[] {
  return values.map((item, itemIndex) => (itemIndex === index ? value : item));
}
