import { useState } from "react";
import type {
  DataConfiguration,
  ProjectConfiguration,
} from "../configuration";
import type { ProjectProbeResult } from "../settingsV2Port";
import { settingsRecordFingerprint } from "./settingsDraft";

export function DataSection({
  value,
  onChange: _onChange,
}: {
  readonly value: DataConfiguration;
  readonly onChange: (value: DataConfiguration) => void;
}): React.JSX.Element {
  return (
    <div className="settings-section-stack">
      <p className="section-intro">
        Local SQLite is the only active Chat-history backend in this build.
        Portable sessions, detailed protocol capture, and automatic retention
        are shown disabled so saved Settings never promise behavior the runtime
        does not perform.
      </p>
      <label className="switch-label" htmlFor="portable-history-capability">
        <input
          checked={false}
          disabled
          id="portable-history-capability"
          title="Unavailable: portable session storage is not composed in the current runtime"
          type="checkbox"
        />
        Portable project sessions (not available)
      </label>
      <label className="settings-field" htmlFor="portable-directory">
        Portable directory
        <input
          disabled
          id="portable-directory"
          spellCheck={false}
          title="Inactive compatibility value; portable session storage is not composed in the current runtime"
          type="text"
          value={value.portableDirectory}
        />
      </label>
      <label className="switch-label" htmlFor="detailed-capture">
        <input
          checked={false}
          disabled
          id="detailed-capture"
          title="Unavailable: detailed protocol capture has no runtime writer in this build"
          type="checkbox"
        />
        Detailed local protocol capture (not available)
      </label>
      <div className="settings-grid two-columns">
        <OptionalDaysField
          id="capture-retention"
          label="Detailed capture retention"
          title="Unavailable until detailed protocol capture has a runtime writer"
          value={value.detailedCaptureRetentionDays}
          disabled
        />
        <OptionalDaysField
          id="history-retention"
          label="Local history retention"
          title="Unavailable: automatic local-history deletion is not composed in this build"
          value={value.localHistoryRetentionDays}
          disabled
        />
      </div>
    </div>
  );
}

export function ProjectsSection({
  projects,
  pickFolder,
  confirm,
  onProbe,
  onChange,
}: {
  readonly projects: readonly ProjectConfiguration[];
  readonly pickFolder: () => Promise<string | null>;
  readonly confirm: (title: string, body: string) => Promise<boolean>;
  readonly onProbe: (project: ProjectConfiguration) => Promise<ProjectProbeResult>;
  readonly onChange: (
    update: (
      current: readonly ProjectConfiguration[],
    ) => readonly ProjectConfiguration[],
  ) => void;
}): React.JSX.Element {
  const [error, setError] = useState<string | null>(null);
  const [probing, setProbing] = useState<string | null>(null);
  const [probes, setProbes] = useState<
    Readonly<Record<string, ProjectProbeResult>>
  >({});
  const clearProbe = (projectId: string) => {
    setProbes((current) => {
      if (!(projectId in current)) return current;
      const next = { ...current };
      delete next[projectId];
      return next;
    });
  };
  const addProject = async () => {
    setError(null);
    try {
      const location = await pickFolder();
      if (location === null) return;
      const leaf = location.replace(/[\\/]+$/u, "").split(/[\\/]/u).at(-1);
      const id = localId("project");
      onChange((current) => [
        ...current,
        {
          id,
          name: leaf?.trim() === "" || leaf === undefined ? "Project" : leaf,
          workspace: { kind: "local_directory", location },
          defaultWorkflowId: null,
          portableHistoryEnabled: false,
        },
      ]);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };
  return (
    <div className="settings-section-stack">
      <div className="section-heading-row">
        <p className="section-intro">
          Projects bind a stable Aworkit identity to one explicit workspace.
          Removing a project never removes workspace files.
        </p>
        <button
          title="Choose a workspace folder and add it as an Aworkit project"
          type="button"
          onClick={() => void addProject()}
        >
          Add folder…
        </button>
      </div>
      {error !== null && <p className="field-error" role="alert">{error}</p>}
      {projects.length === 0 ? (
        <p className="settings-empty">No projects configured.</p>
      ) : (
        <div className="settings-record-list">
          {projects.map((project) => (
            <ProjectEditor
              key={project.id}
              project={project}
              probing={probing === project.id}
              probe={
                probes[project.id]?.draftFingerprint ===
                settingsRecordFingerprint(project)
                  ? probes[project.id]
                  : undefined
              }
              onProbe={() => {
                const requestedFingerprint = settingsRecordFingerprint(project);
                setProbing(project.id);
                void onProbe(project)
                  .then((result) =>
                    setProbes((current) => ({
                      ...current,
                      [project.id]: result,
                    })),
                  )
                  .catch((failure: unknown) =>
                    setProbes((current) => ({
                      ...current,
                      [project.id]: {
                        ok: false,
                        projectId: project.id,
                        workspaceKind: project.workspace.kind,
                        resolvedLocation: null,
                        message:
                          failure instanceof Error
                            ? failure.message
                            : String(failure),
                        draftFingerprint: requestedFingerprint,
                      },
                    })),
                  )
                  .finally(() => setProbing(null));
              }}
              onChange={(next) => {
                setError(null);
                clearProbe(project.id);
                onChange((current) =>
                  current.map((item) =>
                    item.id === project.id ? next : item,
                  ),
                );
              }}
              onRemove={() => {
                void confirm(
                  `Remove ${project.name}?`,
                  "Aworkit will remove the project binding. Workspace files will not be deleted.",
                ).then((accepted) => {
                  if (accepted) {
                    setError(null);
                    clearProbe(project.id);
                    onChange((current) =>
                      current.filter(({ id }) => id !== project.id),
                    );
                  }
                }).catch((failure: unknown) =>
                  setError(failure instanceof Error ? failure.message : String(failure)),
                );
              }}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function ProjectEditor({
  project,
  probing,
  probe,
  onProbe,
  onChange,
  onRemove,
}: {
  readonly project: ProjectConfiguration;
  readonly probing: boolean;
  readonly probe: ProjectProbeResult | undefined;
  readonly onProbe: () => void;
  readonly onChange: (project: ProjectConfiguration) => void;
  readonly onRemove: () => void;
}): React.JSX.Element {
  return (
    <section className="settings-record" aria-labelledby={`${project.id}-heading`}>
      <div className="settings-record-heading">
        <div>
          <h3 id={`${project.id}-heading`}>{project.name}</h3>
          <code>{project.id}</code>
        </div>
        <button
          aria-label={`Remove ${project.name}`}
          className="danger-action"
          title={`Remove ${project.name} from Aworkit without deleting workspace files`}
          type="button"
          onClick={onRemove}
        >
          Remove
        </button>
      </div>
      <div className="settings-grid two-columns">
        <label className="settings-field" htmlFor={`${project.id}-name`}>
          Project name
          <input
            id={`${project.id}-name`}
            title="Name shown in Aworkit navigation and Chat scope"
            type="text"
            value={project.name}
            onChange={(event) => onChange({ ...project, name: event.target.value })}
          />
        </label>
        <label className="settings-field" htmlFor={`${project.id}-kind`}>
          Workspace kind
          <select
            id={`${project.id}-kind`}
            title="How this prepared workspace is provided to Aworkit"
            value={project.workspace.kind}
            onChange={(event) =>
              onChange({
                ...project,
                workspace: {
                  ...project.workspace,
                  kind: event.target.value as ProjectConfiguration["workspace"]["kind"],
                },
              })
            }
          >
            <option value="local_directory">Local directory</option>
            <option value="git_worktree">Git worktree</option>
            <option value="container_mount">Container mount</option>
            {project.workspace.kind === "remote" && (
              <option disabled value="remote">
                Remote prepared workspace (adapter not installed)
              </option>
            )}
          </select>
        </label>
      </div>
      <label className="settings-field" htmlFor={`${project.id}-location`}>
        Workspace location
        <input
          id={`${project.id}-location`}
          spellCheck={false}
          title="Exact workspace location; local paths are canonicalized and revalidated by the trusted core"
          type="text"
          value={project.workspace.location}
          onChange={(event) =>
            onChange({
              ...project,
              workspace: { ...project.workspace, location: event.target.value },
            })
          }
        />
      </label>
      <div className="provider-actions">
        <button
          disabled={probing}
          title="Resolve and validate this exact unsaved workspace draft without granting workflow authority"
          type="button"
          onClick={onProbe}
        >
          {probing ? "Testing…" : "Test workspace"}
        </button>
        {probe !== undefined && (
          <p
            className={`provider-detail ${probe.ok ? "ready" : "error"}`}
            role="status"
          >
            {probe.message}
            {probe.resolvedLocation === null ? "" : ` · ${probe.resolvedLocation}`}
          </p>
        )}
      </div>
      <label className="switch-label" htmlFor={`${project.id}-portable`}>
        <input
          checked={false}
          disabled
          id={`${project.id}-portable`}
          title="Unavailable: current project Chats use local SQLite history only"
          type="checkbox"
        />
        Portable history for new Chats (not available)
      </label>
    </section>
  );
}

function OptionalDaysField({
  id,
  label,
  title,
  value,
  disabled = false,
}: {
  readonly id: string;
  readonly label: string;
  readonly title: string;
  readonly value: number | null | undefined;
  readonly disabled?: boolean;
}): React.JSX.Element {
  return (
    <label className="settings-field" htmlFor={id}>
      {label}
      <input
        disabled={disabled}
        id={id}
        min={1}
        placeholder="Keep indefinitely"
        title={title}
        type="number"
        value={value ?? ""}
      />
    </label>
  );
}

function localId(scope: string): string {
  const random = globalThis.crypto?.randomUUID?.().replaceAll("-", "");
  return `${scope}.${random ?? `${Date.now()}${Math.random().toString(16).slice(2)}`}`;
}
