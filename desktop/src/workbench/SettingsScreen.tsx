import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { NativePresentationAdapter } from "../adapters/contracts";
import type { SettingsLeaveGuard } from "../shell/settingsNavigation";
import { useSettingsFeedback } from "./settings-v2/useSettingsFeedback";
import { useSettingsDiagnostics } from "./settings-v2/useSettingsDiagnostics";
import { settingsSaveContentIssue } from "./settings-v2/settingsSavePostcondition";
import { useSettingsLeave } from "./settings-v2/useSettingsLeave";
import { SettingsLeaveDialog } from "./settings-v2/SettingsLeaveDialog";
import { projectAppearancePreference } from "./appearance";
import type {
  ExtensionConfiguration,
  SettingsConfigurationV2,
  SettingsV2Snapshot,
} from "./configuration";
import { AppearanceSection } from "./settings-v2/AppearanceSection";
import { ApprovalsSection } from "./settings-v2/ApprovalsSection";
import {
  CredentialsSection,
  ToolsSection,
  type CredentialWriteDraft,
} from "./settings-v2/CredentialsToolsSection";
import {
  DataSection,
  ProjectsSection,
} from "./settings-v2/DataProjectsSection";
import {
  ExtensionsSection,
  ExternalAgentsSection,
  McpServersSection,
} from "./settings-v2/IntegrationSections";
import {
  ModelTiersSection,
  ProvidersModelsSection,
} from "./settings-v2/ProviderTierSections";
import { SettingsFieldValidationBoundary } from "./settings-v2/SettingsFields";
import {
  SETTINGS_SECTIONS,
  credentialReferencePaths,
  dirtySettingsSections,
  mcpDraftFingerprint,
  providerDraftFingerprint,
  rebaseSettingsDraft,
  reconcileCredentialReplacementDraft,
  settingsRecordFingerprint,
  settingsDraftIssues,
  type SettingsSectionId,
  type SettingsUiIssue,
} from "./settings-v2/settingsDraft";
import {
  createSettingsV2CorePort,
  nextSettingsV2CommandId,
  type CredentialDeleteCommand,
  type CredentialStoreCommand,
  type CredentialStoreReceipt,
  type ExtensionRegisterCommand,
  type SettingsV2CorePort,
  type SettingsV2Receipt,
} from "./settingsV2Port";

type SettingsPresentation = Pick<
  NativePresentationAdapter,
  "confirm" | "pickFile" | "pickFolder"
>;

type SettingsDraftReconciler = (
  rebasedDraft: SettingsConfigurationV2,
  previousCanonical: SettingsConfigurationV2,
  latestCanonical: SettingsConfigurationV2,
) => SettingsConfigurationV2;

type SettingsSnapshotPostcondition = (
  latest: SettingsV2Snapshot,
) => string | null;

/** Complete, version-checked editor for the canonical Settings v2 document. */
export function SettingsScreen({
  settingsPort,
  presentation,
  active = true,
  visit = 0,
  onBack,
  returnLabel = "Back to Chat",
  registerLeaveGuard,
}: {
  readonly settingsPort?: SettingsV2CorePort;
  readonly presentation?: SettingsPresentation;
  readonly active?: boolean;
  readonly visit?: number;
  readonly onBack?: () => void;
  readonly returnLabel?: string;
  readonly registerLeaveGuard?: (guard: SettingsLeaveGuard | null) => void;
}): React.JSX.Element {
  const port = useMemo(
    () => settingsPort ?? createSettingsV2CorePort(),
    [settingsPort],
  );
  const nativePresentation = presentation ?? inertPresentation;
  const [section, setSection] = useState<SettingsSectionId>("providers");
  const [snapshot, setSnapshot] = useState<SettingsV2Snapshot | null>(null);
  const [draft, setDraft] = useState<SettingsConfigurationV2 | null>(null);
  const [jsonErrors, setJsonErrors] = useState<
    Readonly<Record<string, string>>
  >({});
  const [editorEpoch, setEditorEpoch] = useState(0);
  const [credentialDiagnosticEpoch, setCredentialDiagnosticEpoch] = useState(0);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<
    "save" | "discard" | "credential" | "extension" | null
  >(null);
  const setBanner = useSettingsFeedback(`settings:${visit}`, active, busy);
  const mutationVerified = useRef(false);
  const [credentialDirty, setCredentialDirty] = useState(false);
  const [retryCommandId, setRetryCommandId] = useState<string | null>(null);
  const snapshotRef = useRef<SettingsV2Snapshot | null>(null);
  const draftRef = useRef<SettingsConfigurationV2 | null>(null);
  const draftGenerationRef = useRef(0);
  const renderedDraftGeneration = draftGenerationRef.current;
  const credentialMutationGenerationRef = useRef(0);
  const runDiagnostic = useSettingsDiagnostics(`settings:${visit}`, active, JSON.stringify([draft, credentialDiagnosticEpoch]), renderedDraftGeneration);
  const settingsMutationInFlightRef = useRef(false);
  useEffect(() => {
    if (!active) {
      draftGenerationRef.current += 1;
      setEditorEpoch(value => value + 1);
    }
    return () => { draftGenerationRef.current += 1; };
  }, [active]);

  const runDraftScoped = useCallback(
    async <Result,>(
      operation: () => Promise<Result>,
      staleFallback?: () => Result,
    ): Promise<Result> => {
      const startingGeneration = draftGenerationRef.current;
      const result = await operation();
      if (startingGeneration !== draftGenerationRef.current) {
        if (staleFallback !== undefined) return staleFallback();
        throw new Error(
          "The Settings draft was replaced while this operation was running. Its stale completion was ignored.",
        );
      }
      return result;
    },
    [],
  );
  const draftScopedConfirm = useCallback(
    (title: string, body: string) =>
      runDraftScoped(
        () => nativePresentation.confirm(title, body),
        () => false,
      ),
    [nativePresentation, runDraftScoped],
  );
  const draftScopedPickFolder = useCallback(
    () =>
      runDraftScoped(
        () => nativePresentation.pickFolder(),
        () => null,
      ),
    [nativePresentation, runDraftScoped],
  );
  const draftScopedPickFile = useCallback(
    () =>
      runDraftScoped(
        () => nativePresentation.pickFile(),
        () => null,
      ),
    [nativePresentation, runDraftScoped],
  );

  const applySnapshot = useCallback(
    (
      latest: SettingsV2Snapshot,
      preserveDraft: boolean,
      reconcileDraft?: SettingsDraftReconciler,
    ) => {
      const previous = snapshotRef.current;
      if (previous !== null && latest.version < previous.version) return false;
      const localDraft = draftRef.current;
      const rebasedDraft =
        preserveDraft && previous !== null && localDraft !== null
          ? rebaseSettingsDraft(
              latest.settings,
              localDraft,
              dirtySettingsSections(localDraft, previous.settings),
            )
          : structuredClone(latest.settings);
      const nextDraft =
        reconcileDraft !== undefined && previous !== null
          ? reconcileDraft(
              rebasedDraft,
              previous.settings,
              latest.settings,
            )
          : rebasedDraft;
      snapshotRef.current = latest;
      draftRef.current = nextDraft;
      setSnapshot(latest);
      setDraft(nextDraft);
      if (!preserveDraft) {
        draftGenerationRef.current += 1;
        setJsonErrors({});
        setEditorEpoch((value) => value + 1);
      }
      projectAppearancePreference(
        nextDraft.appearance.mode,
        nextDraft.appearance.fontScale,
      );
      return true;
    },
    [],
  );

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const latest = await port.snapshot();
      if (applySnapshot(latest, false)) setBanner(null);
    } catch (failure) {
      setBanner({ tone: "error", message: failureMessage(failure) });
    } finally {
      setLoading(false);
    }
  }, [applySnapshot, port, setBanner]);

  useEffect(() => {
    if (!active) return;
    let current = true;
    setLoading(true);
    void port
      .snapshot()
      .then((latest) => {
        if (!current) return;
        if (applySnapshot(latest, false)) setBanner(null);
      })
      .catch((failure: unknown) => {
        if (!current) return;
        setBanner({ tone: "error", message: failureMessage(failure) });
      })
      .finally(() => {
        if (current) setLoading(false);
      });
    return () => {
      current = false;
    };
  }, [applySnapshot, port, setBanner, visit, active]);

  const dirtySections = useMemo(
    () =>
      draft === null || snapshot === null
        ? new Set<SettingsSectionId>()
        : dirtySettingsSections(draft, snapshot.settings),
    [draft, snapshot],
  );
  const issues = useMemo(
    () => (draft === null ? [] : settingsDraftIssues(draft, jsonErrors)),
    [draft, jsonErrors],
  );
  const canSave =
    draft !== null &&
    snapshot !== null &&
    busy === null &&
    dirtySections.size > 0 &&
    issues.length === 0;

  const updateDraft = useCallback(
    (update: (current: SettingsConfigurationV2) => SettingsConfigurationV2) => {
      if (settingsMutationInFlightRef.current) {
        setBanner({
          tone: "warning",
          message:
            "Settings edits are locked until the current mutation finishes; no edit was applied.",
        });
        return;
      }
      setRetryCommandId(null);
      setBanner(null);
      const current = draftRef.current;
      if (current === null) return;
      const next = update(current);
      draftRef.current = next;
      setDraft(next);
    },
    [setBanner],
  );

  const updateRenderedDraft = useCallback(
    (update: (current: SettingsConfigurationV2) => SettingsConfigurationV2) => {
      // A promise continuation owned by the previous editor tree can run in
      // the microtask between a full replacement incrementing the generation
      // and React committing the replacement render. Bind child updaters to
      // their render generation so an old callback cannot write in that gap.
      if (renderedDraftGeneration !== draftGenerationRef.current) return;
      updateDraft(update);
    },
    [renderedDraftGeneration, updateDraft],
  );

  const refreshAfterMutation = useCallback(
    async (
      receipt: SettingsV2Receipt,
      preserveDraft: boolean,
      reconcileDraft?: SettingsDraftReconciler,
      snapshotPostcondition?: SettingsSnapshotPostcondition,
    ) => {
      if (!receipt.accepted) {
        throw new Error(receipt.reason ?? "The trusted core rejected the command.");
      }
      const latest = await port.snapshot();
      const postconditionIssue =
        snapshotPostcondition?.(latest) ??
        settingsSnapshotVersionPostconditionIssue(latest, receipt);
      if (postconditionIssue !== null) throw new Error(postconditionIssue);
      applySnapshot(latest, preserveDraft, reconcileDraft);
      mutationVerified.current = true;
      setRetryCommandId(null);
      setBanner({
        tone: receipt.reason === null ? "success" : "warning",
        message: receipt.reason ?? "Settings saved.",
      });
    },
    [applySnapshot, port, setBanner],
  );

  const reconcileFailure = useCallback(
    async (
      failure: unknown,
      attemptedSettings?: SettingsConfigurationV2,
      expectedVersion?: number,
      reconcileDraft?: SettingsDraftReconciler,
      snapshotPostcondition?: SettingsSnapshotPostcondition,
    ): Promise<boolean> => {
      const message = failureMessage(failure);
      try {
        const latest = await port.snapshot();
        const postconditionIssue = snapshotPostcondition?.(latest) ?? null;
        if (postconditionIssue !== null) throw new Error(postconditionIssue);
        if (
          attemptedSettings !== undefined &&
          expectedVersion !== undefined &&
          latest.version > expectedVersion &&
          sameSettings(latest.settings, attemptedSettings)
        ) {
          applySnapshot(latest, false);
          mutationVerified.current = true;
          setRetryCommandId(null);
          setBanner({
            tone: "success",
            message: "Settings were committed; the lost response was reconciled from the canonical snapshot.",
          });
          return true;
        }
        if (
          snapshotRef.current === null ||
          latest.version > snapshotRef.current.version
        ) {
          applySnapshot(latest, true, reconcileDraft);
          setRetryCommandId(null);
          setBanner({
            tone: "warning",
            message: `${message} A newer canonical version was loaded and your edited sections were preserved. Review and Save again.`,
          });
          return true;
        }
      } catch {
        // Preserve both the complete draft and retry command ID after uncertainty.
      }
      setBanner({
        tone: "error",
        message: `${message} Your complete unsaved draft remains available.`,
      });
      return false;
    },
    [applySnapshot, port, setBanner],
  );

  const save = async (): Promise<boolean> => {
    const currentDraft = draftRef.current;
    const currentSnapshot = snapshotRef.current;
    if (!canSave || currentDraft === null || currentSnapshot === null) return false;
    const commandId = retryCommandId ?? nextSettingsV2CommandId();
    const command = {
      commandId,
      expectedVersion: currentSnapshot.version,
      settings: structuredClone(currentDraft),
    };
    if (settingsMutationInFlightRef.current) return false;
    mutationVerified.current = false;
    settingsMutationInFlightRef.current = true;
    setRetryCommandId(commandId);
    setBusy("save");
    setBanner(null);
    let acceptedReceipt: SettingsV2Receipt | null = null;
    try {
      const receipt = await port.commit(command);
      if (receipt.accepted) {
        const receiptIssue = settingsMutationReceiptProofIssue(
          receipt,
          command.commandId,
          command.expectedVersion,
          "Settings save",
        );
        if (receiptIssue !== null) {
          setBanner({
            tone: "error",
            message: `${receiptIssue} Your complete unsaved draft remains available.`,
          });
          return false;
        }
        acceptedReceipt = receipt;
      }
      await refreshAfterMutation(receipt, false, undefined, latest =>
        settingsSnapshotVersionPostconditionIssue(latest, receipt) ?? settingsSaveContentIssue(latest.settings, command.settings));
    } catch (failure) {
      const receiptToRecover = acceptedReceipt;
      await reconcileFailure(
        failure,
        command.settings,
        command.expectedVersion,
        undefined,
        receiptToRecover === null
          ? undefined
          : (latest) =>
              settingsSnapshotVersionPostconditionIssue(
                latest,
                receiptToRecover,
              ) ?? settingsSaveContentIssue(latest.settings, command.settings),
      );
    } finally {
      settingsMutationInFlightRef.current = false;
      setBusy(null);
    }
    return mutationVerified.current;
  };

  const discard = async (confirm = true): Promise<boolean> => {
    if (snapshotRef.current === null || draftRef.current === null) return false;
    if (settingsMutationInFlightRef.current) return false;
    mutationVerified.current = false;
    settingsMutationInFlightRef.current = true;
    setBusy("discard");
    try {
      const accepted = !confirm || await nativePresentation.confirm(
        "Discard unsaved settings?",
        "Every local settings edit, including invalid JSON text, will be restored to the latest canonical version.",
      );
      if (!accepted) return false;
      const latest = await port.snapshot();
      if (!applySnapshot(latest, false)) throw new Error("The latest Settings version could not be confirmed. Your draft was preserved.");
      setRetryCommandId(null);
      setBanner(null);
      mutationVerified.current = true;
    } catch (failure) {
      setBanner({ tone: "error", message: failureMessage(failure) });
    } finally {
      settingsMutationInFlightRef.current = false;
      setBusy(null);
    }
    return mutationVerified.current;
  };

  const leave = useSettingsLeave({
    dirty: dirtySections.size > 0 || Object.keys(jsonErrors).length > 0 || credentialDirty,
    busy: busy !== null, mutationVerified: mutationVerified.current, save, discard,
  }, registerLeaveGuard);

  const storeCredential = async (secretDraft: CredentialWriteDraft) => {
    const currentSnapshot = snapshotRef.current;
    if (currentSnapshot === null) throw new Error("Settings are not loaded.");
    if (settingsMutationInFlightRef.current)
      throw new Error("Another Settings mutation is already in progress.");
    const command: CredentialStoreCommand = {
      commandId: nextSettingsV2CommandId(),
      expectedVersion: currentSnapshot.version,
      ...secretDraft,
    };
    settingsMutationInFlightRef.current = true;
    mutationVerified.current = false;
    credentialMutationGenerationRef.current += 1;
    setBusy("credential");
    setCredentialDiagnosticEpoch((value) => value + 1);
    try {
      let receipt: CredentialStoreReceipt;
      try {
        receipt = await port.storeCredential(command);
      } catch (initialFailure) {
        try {
          // The exact command ID and payload are the proof boundary: native
          // idempotency returns the committed receipt after a lost response,
          // while a real version conflict repeats without mutation.
          receipt = await port.storeCredential(command);
        } catch {
          await reconcileFailure(initialFailure);
          throw initialFailure;
        }
      }
      const receiptIssue = credentialReceiptProofIssue(receipt, command);
      if (receiptIssue !== null) {
        const proofFailure = new Error(receiptIssue);
        await reconcileFailure(proofFailure);
        throw proofFailure;
      }
      const reconcileReplacement: SettingsDraftReconciler = (
        rebasedDraft,
        previousCanonical,
        latestCanonical,
      ) =>
        reconcileCredentialReplacementDraft(
          rebasedDraft,
          secretDraft.replaceCredentialRef,
          receipt.credentialMutation.freshCredentialRef,
          previousCanonical,
          latestCanonical,
        );
      const snapshotPostcondition: SettingsSnapshotPostcondition = (latest) =>
        credentialSnapshotPostconditionIssue(latest, receipt);
      try {
        await refreshAfterMutation(
          receipt,
          true,
          reconcileReplacement,
          snapshotPostcondition,
        );
      } catch (refreshFailure) {
        const recovered = await reconcileFailure(
          refreshFailure,
          undefined,
          undefined,
          reconcileReplacement,
          snapshotPostcondition,
        );
        if (!recovered) throw refreshFailure;
      }
    } finally {
      settingsMutationInFlightRef.current = false;
      setBusy(null);
    }
  };

  const deleteCredential = async (credentialRef: string) => {
    const currentSnapshot = snapshotRef.current;
    const currentDraft = draftRef.current;
    if (currentSnapshot === null || currentDraft === null)
      throw new Error("Settings are not loaded.");
    if (settingsMutationInFlightRef.current)
      throw new Error("Another Settings mutation is already in progress.");
    const references = credentialReferencePaths(currentDraft, credentialRef);
    if (references.length > 0) {
      const preview = references.slice(0, 3).join(", ");
      const remainder = references.length - Math.min(references.length, 3);
      throw new Error(
        `Remove this credential from ${preview}${remainder > 0 ? ` and ${remainder} more consumer${remainder === 1 ? "" : "s"}` : ""}, then Save configuration before deleting it. Your unsaved edits were preserved.`,
      );
    }
    const command: CredentialDeleteCommand = {
      commandId: nextSettingsV2CommandId(),
      expectedVersion: currentSnapshot.version,
      credentialRef,
    };
    settingsMutationInFlightRef.current = true;
    mutationVerified.current = false;
    credentialMutationGenerationRef.current += 1;
    setBusy("credential");
    setCredentialDiagnosticEpoch((value) => value + 1);
    try {
      let receipt: SettingsV2Receipt;
      try {
        receipt = await port.deleteCredential(command);
      } catch (initialFailure) {
        try {
          receipt = await port.deleteCredential(command);
        } catch {
          await reconcileFailure(initialFailure);
          throw initialFailure;
        }
      }
      const receiptIssue = settingsMutationReceiptProofIssue(
        receipt,
        command.commandId,
        command.expectedVersion,
        "credential deletion",
      );
      if (receiptIssue !== null) {
        const proofFailure = new Error(receiptIssue);
        await reconcileFailure(proofFailure);
        throw proofFailure;
      }
      try {
        await refreshAfterMutation(receipt, true);
      } catch (refreshFailure) {
        const recovered = await reconcileFailure(
          refreshFailure,
          undefined,
          undefined,
          undefined,
          (latest) =>
            settingsSnapshotVersionPostconditionIssue(latest, receipt),
        );
        if (!recovered) throw refreshFailure;
      }
    } finally {
      settingsMutationInFlightRef.current = false;
      setBusy(null);
    }
  };

  const registerExtension = async (extension: ExtensionConfiguration) => {
    const currentSnapshot = snapshotRef.current;
    const currentDraft = draftRef.current;
    if (currentSnapshot === null || currentDraft === null)
      throw new Error("Settings are not loaded.");
    if (
      dirtySettingsSections(currentDraft, currentSnapshot.settings).size > 0
    ) {
      throw new Error(
        "Save every settings change, including this discovery, before registering the installed package.",
      );
    }
    const canonical = currentSnapshot.settings.extensions.find(
      ({ id }) => id === extension.id,
    );
    if (canonical?.status !== "discovered") {
      throw new Error(
        "This extension is not a saved discovered package. Discover and Save it first.",
      );
    }
    const command: ExtensionRegisterCommand = {
      commandId: nextSettingsV2CommandId(),
      expectedVersion: currentSnapshot.version,
      extensionId: extension.id,
    };
    if (settingsMutationInFlightRef.current)
      throw new Error("Another Settings mutation is already in progress.");
    settingsMutationInFlightRef.current = true;
    mutationVerified.current = false;
    setBusy("extension");
    let acceptedReceipt: SettingsV2Receipt | null = null;
    let receiptProofIssue: string | null = null;
    try {
      const receipt = await port.registerExtension(command);
      if (receipt.accepted) {
        receiptProofIssue = settingsMutationReceiptProofIssue(
          receipt,
          command.commandId,
          command.expectedVersion,
          "extension registration",
        );
        if (receiptProofIssue !== null) throw new Error(receiptProofIssue);
        acceptedReceipt = receipt;
      }
      await refreshAfterMutation(receipt, false);
    } catch (failure) {
      if (receiptProofIssue !== null) {
        setBanner({
          tone: "error",
          message: `${receiptProofIssue} The saved discovery remains unchanged.`,
        });
        throw failure;
      }
      const receiptToRecover = acceptedReceipt;
      const recovered = await reconcileFailure(
        failure,
        undefined,
        undefined,
        undefined,
        receiptToRecover === null
          ? undefined
          : (latest) =>
              settingsSnapshotVersionPostconditionIssue(
                latest,
                receiptToRecover,
              ),
      );
      if (receiptToRecover === null || !recovered) throw failure;
    } finally {
      settingsMutationInFlightRef.current = false;
      setBusy(null);
    }
  };

  const openIssue = (issue: SettingsUiIssue) => {
    setSection(issue.section);
    window.requestAnimationFrame(() => {
      const explicit =
        issue.focusId === undefined
          ? null
          : document.getElementById(issue.focusId);
      const panel = document.getElementById(`settings-panel-${issue.section}`);
      const fallback = panel?.querySelector<HTMLElement>(
        'input:not(:disabled), select:not(:disabled), textarea:not(:disabled), button:not(:disabled)',
      );
      (explicit ?? fallback)?.focus();
    });
  };

  return (
    <section className="settings-workspace">
      <header className="surface-toolbar">
        <div className="settings-title-actions">
          {onBack && <button type="button" title={returnLabel} aria-label={returnLabel} onClick={onBack}>← Back</button>}
          <div>
          <p className="eyebrow">AWORKIT</p>
          <h1>Settings</h1>
          </div>
        </div>
        <div className="toolbar-actions settings-toolbar-actions">
          <span>
            {snapshot === null ? "Version —" : `Version ${snapshot.version}`}
            {dirtySections.size > 0
              ? ` · ${dirtySections.size} unsaved ${dirtySections.size === 1 ? "section" : "sections"}`
              : " · saved"}
          </span>
          <button
            disabled={dirtySections.size === 0 || busy !== null}
            title="Restore every edited settings section to the latest canonical version"
            type="button"
            onClick={() => void discard()}
          >
            {busy === "discard" ? "Discarding…" : "Discard"}
          </button>
          <button
            className="primary-action"
            disabled={!canSave}
            title={saveTitle(dirtySections.size, issues, busy)}
            type="button"
            onClick={() => void save()}
          >
            {busy === "save" ? "Saving…" : "Save configuration"}
          </button>
        </div>
      </header>
      {active && leave.prompt && <SettingsLeaveDialog busy={leave.deciding || busy !== null} canSave={canSave && !credentialDirty} onSave={() => void leave.decide("save")} onDiscard={() => void leave.decide("discard")} onStay={leave.stay} />}
      <div className="settings-body settings-v2-body">
        <nav aria-label="Settings sections">
          {SETTINGS_SECTIONS.map((item) => {
            const issueCount = issues.filter(
              ({ section: issueSection }) => issueSection === item.id,
            ).length;
            return (
              <button
                aria-current={item.id === section ? "page" : undefined}
                key={item.id}
                title={`Open ${item.label}: ${item.description}`}
                type="button"
                onClick={() => setSection(item.id)}
              >
                <span>{item.label}</span>
                <small>{item.description}</small>
                <span className="settings-nav-state" aria-hidden="true">
                  {issueCount > 0
                    ? `${issueCount} issue${issueCount === 1 ? "" : "s"}`
                    : dirtySections.has(item.id)
                      ? "Unsaved"
                      : ""}
                </span>
              </button>
            );
          })}
        </nav>
        <main>
          {loading && draft === null ? (
            <div className="settings-loading" role="status">
              <p>Loading canonical settings…</p>
            </div>
          ) : draft === null || snapshot === null ? (
            <div className="settings-loading">
              <p>Canonical settings could not be loaded.</p>
              <button
                title="Retry loading the complete canonical settings document"
                type="button"
                onClick={() => void load()}
              >
                Retry
              </button>
            </div>
          ) : (
            <fieldset
              aria-busy={busy !== null}
              className="settings-edit-guard"
              disabled={busy !== null}
              title={
                busy !== null
                  ? "Settings edits are locked until the current mutation and canonical refresh finish"
                  : "Editable Settings configuration"
              }
            >
              <SettingsFieldValidationBoundary
                key={editorEpoch}
                onChange={setJsonErrors}
              >
                {issues.length > 0 && (
                  <ValidationSummary issues={issues} onOpen={openIssue} />
                )}
                <SettingsPanel id="providers" selected={section}>
                <ProvidersModelsSection
                  key={`providers-${credentialDiagnosticEpoch}`}
                  providers={draft.providers}
                  credentials={draft.credentials}
                  health={providerHealthForDraft(snapshot, draft)}
                  confirm={draftScopedConfirm}
                  onChange={(updateProviders) =>
                    updateRenderedDraft((current) => ({
                      ...current,
                      providers: [...updateProviders(current.providers)],
                    }))
                  }
                  onDiscover={(provider) =>
                    runDiagnostic("Model discovery", () => runDraftScoped(async () => {
                      const credentialGeneration =
                        credentialMutationGenerationRef.current;
                      const fingerprint = providerDraftFingerprint(provider);
                      const result = await port.discoverModels({
                        provider,
                        replacementCredential: null,
                        useStoredCredential: provider.credentialRef != null,
                        draftFingerprint: fingerprint,
                      });
                      requireCredentialDiagnosticGeneration(
                        credentialGeneration,
                        credentialMutationGenerationRef.current,
                      );
                      requireCurrentProviderDraft(
                        provider.id,
                        fingerprint,
                        draftRef,
                      );
                      if (result.draftFingerprint !== fingerprint)
                        throw new Error(
                          "The native discovery result did not match this provider draft.",
                        );
                      return result;
                    }))
                  }
                  onProbe={(provider, modelId) => runDiagnostic("Provider test", async () => {
                    const credentialGeneration =
                      credentialMutationGenerationRef.current;
                    const fingerprint = providerDraftFingerprint(provider);
                    const result = await runDraftScoped(() =>
                      port.testProvider({
                        provider,
                        modelId,
                        replacementCredential: null,
                        useStoredCredential: provider.credentialRef != null,
                        draftFingerprint: fingerprint,
                      }),
                    );
                    requireCredentialDiagnosticGeneration(
                      credentialGeneration,
                      credentialMutationGenerationRef.current,
                    );
                    requireCurrentProviderDraft(provider.id, fingerprint, draftRef);
                    if (result.draftFingerprint !== fingerprint)
                      throw new Error("The native test result did not match this provider draft.");
                    const latest = await runDraftScoped(() => port.snapshot());
                    requireCredentialDiagnosticGeneration(
                      credentialGeneration,
                      credentialMutationGenerationRef.current,
                    );
                    requireCurrentProviderDraft(
                      provider.id,
                      fingerprint,
                      draftRef,
                    );
                    applySnapshot(latest, true);
                    return result;
                  })}
                />
              </SettingsPanel>
              <SettingsPanel id="model_tiers" selected={section}>
                <ModelTiersSection
                  tiers={draft.modelTiers}
                  providers={draft.providers}
                  onChange={(modelTiers) =>
                    updateRenderedDraft((current) => ({
                      ...current,
                      modelTiers: [...modelTiers],
                    }))
                  }
                />
              </SettingsPanel>
              <SettingsPanel id="credentials" selected={section}>
                <CredentialsSection
                  onDirtyChange={setCredentialDirty}
                  credentials={draft.credentials}
                  providers={snapshot.settings.providers}
                  confirm={draftScopedConfirm}
                  onStore={storeCredential}
                  onDelete={deleteCredential}
                />
              </SettingsPanel>
              <SettingsPanel id="tools" selected={section}>
                {section === "tools" && <p className="settings-field-help">Tool approvals follow the mode selected in Chat. Manage defaults and saved project approvals under Approvals.</p>}
                <ToolsSection
                  tools={draft.tools}
                  credentials={draft.credentials}
                  projects={draft.projects}
                  onChange={(tools) =>
                    updateRenderedDraft((current) => ({
                      ...current,
                      tools: [...tools],
                    }))
                  }
                  onProbe={(tool, project) => runDiagnostic("Tool test", async () => {
                    const fingerprint = settingsRecordFingerprint({ tool, project });
                    const result = await port.probeTool({
                      tool,
                      project,
                      draftFingerprint: fingerprint,
                    });
                    requireCurrentToolDraft(
                      tool.id,
                      project?.id ?? null,
                      fingerprint,
                      draftRef,
                    );
                    if (result.draftFingerprint !== fingerprint)
                      throw new Error(
                        "The native tool result did not match this tool/project draft.",
                      );
                    return result;
                  })}
                />
              </SettingsPanel>
              <SettingsPanel id="extensions" selected={section}>
                <ExtensionsSection
                  extensions={draft.extensions}
                  onChange={(updateExtensions) =>
                    updateRenderedDraft((current) => ({
                      ...current,
                      extensions: [
                        ...updateExtensions(current.extensions),
                      ],
                    }))
                  }
                  onDiscover={async () => {
                    return runDraftScoped(async () => {
                      const path = await nativePresentation.pickFile();
                      if (path === null) return null;
                      return port.inspectExtension(path);
                    });
                  }}
                  onRegister={registerExtension}
                  registrationBlockedReason={
                    dirtySections.size > 0
                      ? "Save every settings change, including this discovery, before registering the installed package"
                      : undefined
                  }
                />
              </SettingsPanel>
              <SettingsPanel id="mcp" selected={section}>
                <McpServersSection
                  key={`mcp-${credentialDiagnosticEpoch}`}
                  servers={draft.mcpServers}
                  credentials={draft.credentials}
                  onPickCommand={draftScopedPickFile}
                  onChange={(mcpServers) =>
                    updateRenderedDraft((current) => ({
                      ...current,
                      mcpServers: [...mcpServers],
                    }))
                  }
                  onProbe={(server) => runDiagnostic("MCP test", async () => {
                    const fingerprint = mcpDraftFingerprint(server);
                    const result = await port.probeMcp({
                      server,
                      draftFingerprint: fingerprint,
                    });
                    requireCurrentMcpDraft(server.id, fingerprint, draftRef);
                    if (result.draftFingerprint !== fingerprint)
                      throw new Error(
                        "The native MCP result did not match this server draft.",
                      );
                    return {
                      ok: result.protocolVersion !== "unavailable",
                      message: `${result.message} (${result.latencyMillis} ms)`,
                      draftFingerprint: result.draftFingerprint,
                      details: [
                        ...result.toolNames.map((name) => `Tool: ${name}`),
                        ...result.resourceNames.map(
                          (name) => `Resource: ${name}`,
                        ),
                        ...result.promptNames.map((name) => `Prompt: ${name}`),
                      ],
                    };
                  })}
                />
              </SettingsPanel>
              <SettingsPanel id="external_agents" selected={section}>
                <ExternalAgentsSection
                  key={`external-agents-${credentialDiagnosticEpoch}`}
                  agents={draft.externalAgents}
                  mcpServers={draft.mcpServers}
                  credentials={draft.credentials}
                  onChange={(externalAgents) =>
                    updateRenderedDraft((current) => ({
                      ...current,
                      externalAgents: [...externalAgents],
                    }))
                  }
                  onProbe={(agent) => runDiagnostic("External agent test", async () => {
                    const fingerprint = settingsRecordFingerprint(agent);
                    const result = await port.probeExternalAgent({
                      agent,
                      draftFingerprint: fingerprint,
                    });
                    requireCurrentExternalAgentDraft(
                      agent.id,
                      fingerprint,
                      draftRef,
                    );
                    if (result.draftFingerprint !== fingerprint)
                      throw new Error(
                        "The native external-agent result did not match this agent draft.",
                      );
                    const platform = [result.platformFamily, result.platformOs]
                      .filter((value): value is string => value !== null)
                      .join(" / ");
                    return {
                      ok: result.protocol !== "unavailable",
                      message: `${result.message} (${result.latencyMillis} ms)`,
                      draftFingerprint: result.draftFingerprint,
                      details: [
                        `Protocol: ${result.protocol}`,
                        ...(result.serverIdentity === null
                          ? []
                          : [`Server: ${result.serverIdentity}`]),
                        ...(platform.length === 0 ? [] : [`Platform: ${platform}`]),
                        ...(result.accountType === null
                          ? []
                          : [`Account: ${result.accountType}`]),
                        ...result.modelIds.map((modelId) => `Model: ${modelId}`),
                      ],
                      capabilities: result.capabilities,
                    };
                  })}
                />
              </SettingsPanel>
              <SettingsPanel id="data" selected={section}>
                <DataSection
                  value={draft.data}
                  onChange={(data) =>
                    updateRenderedDraft((current) => ({ ...current, data }))
                  }
                />
              </SettingsPanel>
              <SettingsPanel id="projects" selected={section}>
                <ProjectsSection
                  projects={draft.projects}
                  pickFolder={draftScopedPickFolder}
                  confirm={draftScopedConfirm}
                  onProbe={(project) => runDiagnostic("Project test", async () => {
                    const fingerprint = settingsRecordFingerprint(project);
                    const result = await port.probeProject({
                      project,
                      draftFingerprint: fingerprint,
                    });
                    requireCurrentProjectDraft(
                      project.id,
                      fingerprint,
                      draftRef,
                    );
                    if (result.draftFingerprint !== fingerprint)
                      throw new Error(
                        "The native project result did not match this project draft.",
                      );
                    return result;
                  })}
                  onChange={(updateProjects) =>
                    updateRenderedDraft((current) => ({
                      ...current,
                      projects: [...updateProjects(current.projects)],
                    }))
                  }
                />
              </SettingsPanel>
              <SettingsPanel id="approvals" selected={section}>
                {section === "approvals" && <ApprovalsSection mode={draft.approvals.defaultMode}
                  onChange={defaultMode => updateRenderedDraft(current => ({ ...current, approvals: { defaultMode } }))} />}
              </SettingsPanel>
              <SettingsPanel id="appearance" selected={section}>
                <AppearanceSection
                  value={draft.appearance}
                  onChange={(appearance) =>
                    updateRenderedDraft((current) => ({ ...current, appearance }))
                  }
                  onReset={() => {
                    const appearance = { mode: "system" as const, fontScale: 1 };
                    projectAppearancePreference(appearance.mode, appearance.fontScale);
                    updateRenderedDraft((current) => ({ ...current, appearance }));
                  }}
                />
                </SettingsPanel>
              </SettingsFieldValidationBoundary>
            </fieldset>
          )}
        </main>
      </div>
    </section>
  );
}

function SettingsPanel({
  id,
  selected,
  children,
}: {
  readonly id: SettingsSectionId;
  readonly selected: SettingsSectionId;
  readonly children: React.ReactNode;
}): React.JSX.Element {
  const definition = SETTINGS_SECTIONS.find((item) => item.id === id);
  return (
    <section
      aria-labelledby={`settings-heading-${id}`}
      className="settings-v2-panel"
      hidden={selected !== id}
      id={`settings-panel-${id}`}
    >
      <div className="settings-panel-heading">
        <h2 id={`settings-heading-${id}`}>{definition?.label}</h2>
        <p>{definition?.description}</p>
      </div>
      {children}
    </section>
  );
}

function ValidationSummary({
  issues,
  onOpen,
}: {
  readonly issues: readonly SettingsUiIssue[];
  readonly onOpen: (issue: SettingsUiIssue) => void;
}): React.JSX.Element {
  return (
    <section className="settings-validation-summary" aria-labelledby="settings-validation-heading">
      <div>
        <strong id="settings-validation-heading">
          {issues.length} validation {issues.length === 1 ? "issue" : "issues"}
        </strong>
        <span>Save is blocked until every issue is resolved.</span>
      </div>
      <ul>
        {issues.slice(0, 8).map((issue, index) => (
          <li key={`${issue.path}-${issue.message}-${index}`}>
            <button
              title={`Open ${sectionLabel(issue.section)} and focus the relevant setting`}
              type="button"
              onClick={() => onOpen(issue)}
            >
              <strong>{sectionLabel(issue.section)}</strong>
              <span>{issue.message}</span>
            </button>
          </li>
        ))}
      </ul>
      {issues.length > 8 && <p>{issues.length - 8} more issues remain.</p>}
    </section>
  );
}

function providerHealthForDraft(
  snapshot: SettingsV2Snapshot,
  draft: SettingsConfigurationV2,
): SettingsV2Snapshot["providerHealth"] {
  return snapshot.providerHealth.map((health) => {
    const canonical = snapshot.settings.providers.find(
      ({ id }) => id === health.providerId,
    );
    const current = draft.providers.find(({ id }) => id === health.providerId);
    if (canonical === undefined || current === undefined || sameSettings(canonical, current))
      return health;
    return {
      providerId: health.providerId,
      state: current.enabled ? ("configured" as const) : ("disabled" as const),
      detail: "This provider has unsaved changes; its previous health result is not current.",
    };
  });
}

function requireCurrentProviderDraft(
  providerId: string,
  fingerprint: string,
  draftRef: React.RefObject<SettingsConfigurationV2 | null>,
): void {
  const current = draftRef.current?.providers.find(({ id }) => id === providerId);
  if (current === undefined || providerDraftFingerprint(current) !== fingerprint) {
    throw new Error(
      "The provider draft changed while the native operation was running. Its stale result was ignored.",
    );
  }
}

function requireCredentialDiagnosticGeneration(
  startedGeneration: number,
  currentGeneration: number,
): void {
  if (startedGeneration !== currentGeneration)
    throw new Error(
      "A credential mutation started while this provider operation was running. Its stale result and snapshot were ignored.",
    );
}

function requireCurrentMcpDraft(
  serverId: string,
  fingerprint: string,
  draftRef: React.RefObject<SettingsConfigurationV2 | null>,
): void {
  const current = draftRef.current?.mcpServers.find(({ id }) => id === serverId);
  if (current === undefined || mcpDraftFingerprint(current) !== fingerprint)
    throw new Error(
      "This MCP server draft changed while the native probe was running. The result was not applied.",
    );
}

function requireCurrentProjectDraft(
  projectId: string,
  fingerprint: string,
  draftRef: React.RefObject<SettingsConfigurationV2 | null>,
): void {
  const current = draftRef.current?.projects.find(({ id }) => id === projectId);
  if (current === undefined || settingsRecordFingerprint(current) !== fingerprint)
    throw new Error(
      "This project draft changed while the native probe was running. The result was not applied.",
    );
}

function requireCurrentToolDraft(
  toolId: string,
  projectId: string | null,
  fingerprint: string,
  draftRef: React.RefObject<SettingsConfigurationV2 | null>,
): void {
  const tool = draftRef.current?.tools.find(({ id }) => id === toolId);
  const project =
    projectId === null
      ? null
      : draftRef.current?.projects.find(({ id }) => id === projectId) ?? null;
  if (
    tool === undefined ||
    settingsRecordFingerprint({ tool, project }) !== fingerprint
  )
    throw new Error(
      "This tool or project draft changed while the native probe was running. The result was not applied.",
    );
}

function requireCurrentExternalAgentDraft(
  agentId: string,
  fingerprint: string,
  draftRef: React.RefObject<SettingsConfigurationV2 | null>,
): void {
  const agent = draftRef.current?.externalAgents.find(({ id }) => id === agentId);
  if (agent === undefined || settingsRecordFingerprint(agent) !== fingerprint)
    throw new Error(
      "This external-agent draft changed while the native handshake was running. The result was not applied.",
    );
}

function sectionLabel(section: SettingsSectionId): string {
  return SETTINGS_SECTIONS.find(({ id }) => id === section)?.label ?? section;
}

function saveTitle(
  dirtyCount: number,
  issues: readonly SettingsUiIssue[],
  busy: string | null,
): string {
  if (busy !== null) return "Another settings operation is in progress";
  if (issues.length > 0)
    return `Resolve ${issues.length} validation ${issues.length === 1 ? "issue" : "issues"} before saving`;
  if (dirtyCount === 0) return "There are no unsaved settings changes";
  return "Atomically save the complete version-checked settings document";
}

function failureMessage(failure: unknown): string {
  return failure instanceof Error ? failure.message : String(failure);
}

function credentialReceiptProofIssue(
  receipt: CredentialStoreReceipt,
  command: CredentialStoreCommand,
): string | null {
  const settingsProofIssue = settingsMutationReceiptProofIssue(
    receipt,
    command.commandId,
    command.expectedVersion,
    "credential storage",
  );
  if (settingsProofIssue !== null) return settingsProofIssue;
  const expectedOperation =
    command.replaceCredentialRef === null ? "create" : "replace";
  if (receipt.credentialMutation.operation !== expectedOperation)
    return "The credential receipt reported the wrong mutation operation; no draft references were rewritten.";
  if (
    receipt.credentialMutation.previousCredentialRef !==
    command.replaceCredentialRef
  )
    return "The credential receipt did not match the exact previous reference; no draft references were rewritten.";
  if (
    receipt.credentialMutation.freshCredentialRef ===
    command.replaceCredentialRef
  )
    return "The credential receipt did not contain a fresh replacement reference; no draft references were rewritten.";
  return null;
}

function credentialSnapshotPostconditionIssue(
  snapshot: SettingsV2Snapshot,
  receipt: CredentialStoreReceipt,
): string | null {
  if (snapshot.version < receipt.currentVersion)
    return `The canonical credential snapshot is stale (version ${snapshot.version}, expected at least ${receipt.currentVersion}); no draft references were rewritten.`;
  const freshCredentialRef = receipt.credentialMutation.freshCredentialRef;
  if (
    !snapshot.settings.credentials.some(
      ({ credentialRef }) => credentialRef === freshCredentialRef,
    )
  )
    return "The canonical credential snapshot did not contain the exact fresh credential reference from the accepted receipt; no draft references were rewritten.";
  return null;
}

function settingsSnapshotVersionPostconditionIssue(
  snapshot: SettingsV2Snapshot,
  receipt: SettingsV2Receipt,
): string | null {
  if (snapshot.version >= receipt.currentVersion) return null;
  return `The canonical Settings snapshot is stale (version ${snapshot.version}, expected at least ${receipt.currentVersion}); the accepted mutation was not applied locally.`;
}

function settingsMutationReceiptProofIssue(
  receipt: SettingsV2Receipt,
  commandId: string,
  expectedVersion: number,
  operation: string,
): string | null {
  if (!receipt.accepted)
    return receipt.reason ?? `The trusted core rejected ${operation}.`;
  if (receipt.commandId !== commandId)
    return `The ${operation} receipt did not match the exact command ID; no canonical result was applied.`;
  if (receipt.currentVersion !== expectedVersion + 1)
    return `The ${operation} receipt did not prove the exact expected version transition; no canonical result was applied.`;
  return null;
}

function sameSettings(left: unknown, right: unknown): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

const inertPresentation: SettingsPresentation = {
  async confirm(): Promise<boolean> {
    return false;
  },
  async pickFile(): Promise<null> {
    return null;
  },
  async pickFolder(): Promise<null> {
    return null;
  },
};
