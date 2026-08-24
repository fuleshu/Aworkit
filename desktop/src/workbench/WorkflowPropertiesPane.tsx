import { useEffect, useMemo, useState } from "react";
import {
  workflowNodeId,
  type JsonObject,
  type JsonValue,
  type WorkflowDocument,
  type WorkflowSummary,
  type WorkflowValidationIssue,
} from "./workflow";
import type { WorkflowExecutionCompatibility } from "./workflowExecution";

interface WorkflowPropertiesPaneProps {
  readonly document: WorkflowDocument;
  readonly editable: boolean;
  readonly selectedId: string | null;
  readonly selectedNode: JsonObject | null;
  readonly selectedEdge: JsonObject | null;
  readonly summary: WorkflowSummary;
  readonly issues: readonly WorkflowValidationIssue[];
  readonly compatibility: WorkflowExecutionCompatibility;
  readonly onOpenSettings?: () => void;
  readonly onWorkflowField: (key: string, value: JsonValue) => void;
  readonly onNodeProperty: (key: "label" | "type", value: string) => void;
  readonly onNodeField: (key: string, value: JsonValue) => void;
  readonly onRenameNode: (currentId: string, nextId: string) => void;
  readonly onEdgeFields: (patch: JsonObject) => void;
  readonly onDelete: () => void;
  readonly onSelectIssue: (id: string) => void;
  readonly onPendingDraftChange: (pending: boolean) => void;
}

export function WorkflowPropertiesPane({
  document,
  editable,
  selectedId,
  selectedNode,
  selectedEdge,
  summary,
  issues,
  compatibility,
  onOpenSettings,
  onWorkflowField,
  onNodeProperty,
  onNodeField,
  onRenameNode,
  onEdgeFields,
  onDelete,
  onSelectIssue,
  onPendingDraftChange,
}: WorkflowPropertiesPaneProps): React.JSX.Element {
  const heading =
    selectedNode !== null
      ? String(selectedNode.label ?? selectedId ?? "Node")
      : selectedEdge !== null
        ? `Transition ${selectedId ?? ""}`
        : "Workflow";
  return (
    <aside className="properties-pane" aria-label="Workflow properties">
      <header>
        <div>
          <p className="eyebrow">PROPERTIES</p>
          <h2>{heading}</h2>
        </div>
        {(selectedNode !== null || selectedEdge !== null) && (
          <button
            className="danger-action"
            disabled={!editable}
            title={`Delete this ${selectedNode !== null ? "node and its transitions" : "transition"}; Undo can restore it`}
            type="button"
            onClick={onDelete}
          >
            Delete {selectedNode !== null ? "node" : "transition"}
          </button>
        )}
      </header>
      {selectedNode !== null && selectedId !== null ? (
        <NodeProperties
          key={selectedId}
          document={document}
          editable={editable}
          node={selectedNode}
          nodeId={selectedId}
          onDelete={onDelete}
          onField={onNodeField}
          onOpenSettings={onOpenSettings}
          onProperty={onNodeProperty}
          onRename={onRenameNode}
          onPendingDraftChange={onPendingDraftChange}
        />
      ) : selectedEdge !== null ? (
        <EdgeProperties
          document={document}
          edge={selectedEdge}
          editable={editable}
          onFields={onEdgeFields}
        />
      ) : (
        <WorkflowProperties
          compatibility={compatibility}
          document={document}
          editable={editable}
          issues={issues}
          summary={summary}
          onField={onWorkflowField}
          onSelectIssue={onSelectIssue}
        />
      )}
    </aside>
  );
}

function NodeProperties({
  document,
  editable,
  node,
  nodeId,
  onProperty,
  onField,
  onRename,
  onOpenSettings,
  onPendingDraftChange,
}: {
  readonly document: WorkflowDocument;
  readonly editable: boolean;
  readonly node: JsonObject;
  readonly nodeId: string;
  readonly onDelete: () => void;
  readonly onProperty: (key: "label" | "type", value: string) => void;
  readonly onField: (key: string, value: JsonValue) => void;
  readonly onRename: (currentId: string, nextId: string) => void;
  readonly onOpenSettings?: () => void;
  readonly onPendingDraftChange: (pending: boolean) => void;
}): React.JSX.Element {
  const configurationValue = useMemo(
    () => JSON.stringify(node.configuration ?? {}, null, 2),
    [node.configuration],
  );
  const [idDraft, setIdDraft] = useState(nodeId);
  const [configurationDraft, setConfigurationDraft] =
    useState(configurationValue);
  const [idError, setIdError] = useState<string | null>(null);
  const [configurationError, setConfigurationError] = useState<string | null>(
    null,
  );
  useEffect(() => {
    setIdDraft(nodeId);
    setIdError(null);
  }, [nodeId]);
  useEffect(() => {
    setConfigurationDraft(configurationValue);
    setConfigurationError(null);
  }, [nodeId, configurationValue]);
  const pendingDraft =
    idDraft !== nodeId || configurationDraft !== configurationValue;
  useEffect(() => {
    onPendingDraftChange(pendingDraft);
  }, [onPendingDraftChange, pendingDraft]);
  useEffect(
    () => () => {
      onPendingDraftChange(false);
    },
    [onPendingDraftChange],
  );

  const applyId = () => {
    const nextId = idDraft.trim();
    if (nextId === "") {
      setIdError("Node ID is required.");
      return;
    }
    const duplicate = document.nodes.some(
      (candidate, index) =>
        workflowNodeId(candidate, index) !== nodeId && candidate.id === nextId,
    );
    if (duplicate) {
      setIdError(`Node ID ${nextId} is already in use.`);
      return;
    }
    setIdError(null);
    onRename(nodeId, nextId);
  };
  const applyConfiguration = () => {
    try {
      const value: unknown = JSON.parse(configurationDraft);
      if (typeof value !== "object" || value === null || Array.isArray(value))
        throw new Error("Configuration must be a JSON object.");
      setConfigurationError(null);
      onField("configuration", value as JsonValue);
    } catch (failure) {
      setConfigurationError(
        failure instanceof Error ? failure.message : String(failure),
      );
    }
  };

  return (
    <div className="property-content">
      <label>
        Label
        <input
          disabled={!editable}
          title="Edit the selected node label"
          value={String(node.label ?? "")}
          onChange={(event) => onProperty("label", event.target.value)}
        />
      </label>
      <label>
        Node type
        <input
          disabled={!editable}
          title="Edit the stable node type; unknown types remain preserved and exportable"
          value={String(node.type ?? "")}
          onChange={(event) => onProperty("type", event.target.value)}
        />
      </label>
      <div className="workflow-field-with-action">
        <label>
          Node ID
          <input
            aria-invalid={idError !== null}
            disabled={!editable}
            title="Edit the stable node ID and rewrite connected transition references atomically"
            value={idDraft}
            onChange={(event) => setIdDraft(event.target.value)}
          />
        </label>
        <button
          disabled={!editable || idDraft === nodeId}
          title="Apply this node ID and update every connected transition"
          type="button"
          onClick={applyId}
        >
          Apply ID
        </button>
      </div>
      {idError !== null && (
        <p className="field-error" role="alert">
          {idError}
        </p>
      )}
      <label>
        Configuration JSON
        <textarea
          aria-invalid={configurationError !== null}
          disabled={!editable}
          rows={11}
          spellCheck={false}
          title="Edit this node's canonical JSON configuration; provider credentials do not belong here"
          value={configurationDraft}
          onChange={(event) => setConfigurationDraft(event.target.value)}
        />
      </label>
      <button
        disabled={!editable || configurationDraft === configurationValue}
        title="Parse and apply the configuration as one undoable transaction"
        type="button"
        onClick={applyConfiguration}
      >
        Apply configuration
      </button>
      {configurationError !== null && (
        <p className="field-error" role="alert">
          {configurationError}
        </p>
      )}
      <p className="workflow-help-copy">
        Provider endpoints and secrets are configured in Settings. Workflow
        nodes keep only portable logical references.
      </p>
      {node.capabilityStatus === "missing" && (
        <section className="dependency-resolution">
          <span className="status incompatible">Missing</span>
          <p>
            {String(node.requirement ?? node.label ?? nodeId)} is unresolved.
            Importing this document did not install or enable code.
          </p>
          <button
            disabled={onOpenSettings === undefined}
            title="Open Settings to configure a compatible capability"
            type="button"
            onClick={onOpenSettings}
          >
            Configure compatible capability…
          </button>
        </section>
      )}
      <details className="raw-workflow-fields">
        <summary>Preserved node JSON</summary>
        <pre>{JSON.stringify(node, null, 2)}</pre>
      </details>
    </div>
  );
}

function EdgeProperties({
  document,
  edge,
  editable,
  onFields,
}: {
  readonly document: WorkflowDocument;
  readonly edge: JsonObject;
  readonly editable: boolean;
  readonly onFields: (patch: JsonObject) => void;
}): React.JSX.Element {
  return (
    <div className="property-content">
      <label>
        Source node
        <select
          disabled={!editable}
          title="Change the transition source while preserving its other fields"
          value={String(edge.source ?? "")}
          onChange={(event) => onFields({ source: event.target.value })}
        >
          {nodeOptions(document)}
        </select>
      </label>
      <label>
        Target node
        <select
          disabled={!editable}
          title="Change the transition target while preserving its other fields"
          value={String(edge.target ?? "")}
          onChange={(event) => onFields({ target: event.target.value })}
        >
          {nodeOptions(document)}
        </select>
      </label>
      <label>
        Label
        <input
          disabled={!editable}
          title="Edit the optional transition label"
          value={String(edge.label ?? "")}
          onChange={(event) => onFields({ label: event.target.value })}
        />
      </label>
      <details className="raw-workflow-fields">
        <summary>Preserved transition JSON</summary>
        <pre>{JSON.stringify(edge, null, 2)}</pre>
      </details>
    </div>
  );
}

function WorkflowProperties({
  document,
  editable,
  summary,
  issues,
  compatibility,
  onField,
  onSelectIssue,
}: {
  readonly document: WorkflowDocument;
  readonly editable: boolean;
  readonly summary: WorkflowSummary;
  readonly issues: readonly WorkflowValidationIssue[];
  readonly compatibility: WorkflowExecutionCompatibility;
  readonly onField: (key: string, value: JsonValue) => void;
  readonly onSelectIssue: (id: string) => void;
}): React.JSX.Element {
  return (
    <div className="property-content">
      <label>
        Workflow name
        <input
          disabled={!editable}
          title="Edit the workflow display name"
          value={typeof document.name === "string" ? document.name : ""}
          onChange={(event) => onField("name", event.target.value)}
        />
      </label>
      <label>
        Comments
        <textarea
          disabled={!editable}
          rows={4}
          title="Edit workflow comments stored in the canonical JSON document"
          value={typeof document.comments === "string" ? document.comments : ""}
          onChange={(event) => onField("comments", event.target.value)}
        />
      </label>
      <dl>
        <div>
          <dt>Schema version</dt>
          <dd>{document.schemaVersion}</dd>
        </div>
        <div>
          <dt>Nodes</dt>
          <dd>{summary.nodes}</dd>
        </div>
        <div>
          <dt>Transitions</dt>
          <dd>{summary.edges}</dd>
        </div>
        <div>
          <dt>Dependencies</dt>
          <dd>{summary.unresolved} unresolved</dd>
        </div>
      </dl>
      <h3>Document validation</h3>
      {issues.length === 0 ? (
        <p className="success-copy">✓ Canonical document is valid</p>
      ) : (
        <ul className="workflow-validation-list">
          {issues.map((issue, index) => (
            <li key={`${issue.code}-${issue.itemId}-${index}`}>
              <button
                title="Select the node or transition associated with this issue"
                type="button"
                onClick={() => onSelectIssue(issue.itemId)}
              >
                {issue.message}
              </button>
            </li>
          ))}
        </ul>
      )}
      <h3>Native execution</h3>
      {compatibility.executable ? (
        <p className="success-copy">✓ Executable as Simple Chat</p>
      ) : (
        <ul className="workflow-validation-list runtime-limit-list">
          {compatibility.issues.map((issue) => (
            <li key={issue.code}>{issue.message}</li>
          ))}
        </ul>
      )}
      <p className="workflow-help-copy">
        {editable
          ? "Unknown nodes and fields remain inspectable, editable, undoable, savable, and exportable. Native Run stays gated by the current exact Simple Chat contract."
          : "This unsupported or inert schema remains inspectable and losslessly exportable, but this build will not edit or overwrite it."}
      </p>
      <details className="raw-workflow-fields">
        <summary>Complete preserved workflow JSON</summary>
        <pre>{JSON.stringify(document, null, 2)}</pre>
      </details>
    </div>
  );
}

function nodeOptions(document: WorkflowDocument): React.JSX.Element[] {
  return document.nodes.map((node, index) => {
    const id = workflowNodeId(node, index);
    return (
      <option key={id} value={id}>
        {String(node.label ?? id)} ({id})
      </option>
    );
  });
}
