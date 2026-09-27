/**
 * Workflow document file handling for the Workflow designer.
 *
 * The native workflow library stays the one canonical store, so a file is only
 * a copy the user chose. Open and Save As cross the host boundary for exactly
 * the three things a webview must not do itself: ask the operating system for a
 * path, read one chosen document, and write one atomically. Replacing an
 * existing file is a two-step protocol — the host refuses an unconfirmed
 * overwrite, so the designer asks through the application's own confirmation
 * dialog and only then writes again.
 */
import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";

const pathSchema = z.string().min(1);
const writeOutcomeSchema = z.enum(["written", "exists"]);
const confirmationSchema = z.boolean();

/** One complete workflow document written to one chosen path. */
export interface WorkflowFileWrite {
  readonly path: string;
  readonly contents: string;
  /** Set only after the user confirmed replacing an existing file. */
  readonly overwrite: boolean;
}

/** What one write did: it wrote the document, or refused an existing file. */
export type WorkflowFileWriteOutcome = "written" | "exists";

/** Native workflow-file surface: operating-system dialogs plus document IO. */
export interface WorkflowFilePort {
  /** Asks the operating system for a workflow document to open. */
  chooseOpenPath(): Promise<string | null>;
  /** Asks the operating system where a workflow document is saved. */
  chooseSavePath(suggestedName: string): Promise<string | null>;
  /** Reads one chosen workflow document as bounded UTF-8 text. */
  readDocument(path: string): Promise<string>;
  /** Writes one complete document; an existing file needs `overwrite`. */
  writeDocument(command: WorkflowFileWrite): Promise<WorkflowFileWriteOutcome>;
  /** Asks a blocking question through the application's confirmation dialog. */
  confirm(title: string, body: string): Promise<boolean>;
}

export class TauriWorkflowFilePort implements WorkflowFilePort {
  public async chooseOpenPath(): Promise<string | null> {
    return pathSchema.nullable().parse(await invoke("native_workflow_open_path"));
  }
  public async chooseSavePath(suggestedName: string): Promise<string | null> {
    return pathSchema
      .nullable()
      .parse(await invoke("native_workflow_save_path", { suggestedName }));
  }
  public async readDocument(path: string): Promise<string> {
    return z.string().parse(await invoke("native_workflow_read_file", { path }));
  }
  public async writeDocument(
    command: WorkflowFileWrite,
  ): Promise<WorkflowFileWriteOutcome> {
    return writeOutcomeSchema.parse(
      await invoke("native_workflow_write_file", { ...command }),
    );
  }
  public async confirm(title: string, body: string): Promise<boolean> {
    return confirmationSchema.parse(await invoke("native_confirm", { title, body }));
  }
}

/**
 * Honest browser Preview: there is no webview file chooser and no Blob
 * download on this surface, so every file operation reports that it needs the
 * native desktop runtime instead of pretending to have performed one.
 */
export class PreviewWorkflowFilePort implements WorkflowFilePort {
  public async chooseOpenPath(): Promise<string | null> {
    throw new Error(nativeOnly("Opening a workflow file"));
  }
  public async chooseSavePath(_suggestedName: string): Promise<string | null> {
    throw new Error(nativeOnly("Writing a workflow file"));
  }
  public async readDocument(_path: string): Promise<string> {
    throw new Error(nativeOnly("Reading a workflow file"));
  }
  public async writeDocument(
    _command: WorkflowFileWrite,
  ): Promise<WorkflowFileWriteOutcome> {
    throw new Error(nativeOnly("Writing a workflow file"));
  }
  public async confirm(_title: string, _body: string): Promise<boolean> {
    // No file operation can reach a confirmation in Preview, and a question
    // that cannot be asked must never be answered with "yes".
    return false;
  }
}

export function createWorkflowFilePort(): WorkflowFilePort {
  return "__TAURI_INTERNALS__" in window
    ? new TauriWorkflowFilePort()
    : new PreviewWorkflowFilePort();
}

/** Suffix Aworkit suggests for a workflow document it writes itself. */
export const WORKFLOW_FILE_SUFFIX = ".aworkit.json";

/** File-name suggestion for a workflow, derived from its display name. */
export function suggestedWorkflowFileName(workflowName: string): string {
  const name = workflowName
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/gu, "-")
    .replace(/^-|-$/gu, "");
  return `${name === "" ? "workflow" : name}${WORKFLOW_FILE_SUFFIX}`;
}

/** The chosen path's own file name, for labels and suggestions. */
export function workflowFileNameFromPath(path: string): string {
  return path.split(/[\\/]/u).pop() ?? path;
}

/**
 * Display-name suggestion for a document that carries no name of its own: a
 * file basename is only a suggestion, and a stored name always wins on load.
 */
export function workflowNameFromPath(path: string): string {
  const base = workflowFileNameFromPath(path)
    .replace(/\.aworkit\.json$/iu, "")
    .replace(/\.json$/iu, "")
    .replace(/[_-]+/gu, " ")
    .trim();
  return base === "" ? "Untitled workflow" : base;
}

function nativeOnly(action: string): string {
  return `${action} requires the native desktop runtime; this browser Preview keeps workflow documents in the workflow library only.`;
}
