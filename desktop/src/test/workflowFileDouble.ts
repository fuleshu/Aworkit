/**
 * In-memory stand-in for the native workflow-file port.
 *
 * Import and Export cross the host boundary through this port, so a test can
 * decide which path a dialog answers with, which files exist, and whether the
 * user confirms replacing an existing file — without a real operating-system
 * dialog and without touching the test machine's filesystem.
 */
import type {
  WorkflowFilePort,
  WorkflowFileWrite,
  WorkflowFileWriteOutcome,
} from "../workbench/workflowFilePort";

export class WorkflowFileDouble implements WorkflowFilePort {
  /** The paths a test pretends already exist, and their text. */
  public readonly files = new Map<string, string>();
  /** Every write this port was asked for, in order. */
  public readonly writes: {
    readonly path: string;
    readonly contents: string;
    readonly overwrite: boolean;
  }[] = [];
  /** Every question the designer asked through the confirmation dialog. */
  public readonly confirmations: {
    readonly title: string;
    readonly body: string;
  }[] = [];
  /** Names the designer proposed to the export dialog, in order. */
  public readonly suggestedNames: string[] = [];
  /** How often the import dialog was asked for a path. */
  public importCalls = 0;
  /** How often the export dialog was asked for a path. */
  public exportCalls = 0;
  /** What the import dialog answers; `null` is the user cancelling it. */
  public openPath: string | null = null;
  /** What the export dialog answers; `null` is the user cancelling it. */
  public savePath: string | null = null;

  private answers: boolean[] = [];

  /** Seeds a file as if the user had written or received it earlier. */
  public seed(path: string, contents: string): void {
    this.files.set(path, contents);
  }

  /** Queues the answers confirmation dialogs give; the last one repeats. */
  public answer(...values: boolean[]): void {
    this.answers = [...values];
  }

  public async chooseImportPath(): Promise<string | null> {
    this.importCalls += 1;
    return this.openPath;
  }

  public async chooseExportPath(suggestedName: string): Promise<string | null> {
    this.exportCalls += 1;
    this.suggestedNames.push(suggestedName);
    return this.savePath;
  }

  public async readDocument(path: string): Promise<string> {
    const contents = this.files.get(path);
    if (contents === undefined)
      throw new Error(`cannot read ${path}: the file does not exist`);
    return contents;
  }

  public async writeDocument(
    command: WorkflowFileWrite,
  ): Promise<WorkflowFileWriteOutcome> {
    this.writes.push({ ...command });
    if (!command.overwrite && this.files.has(command.path)) return "exists";
    this.files.set(command.path, command.contents);
    return "written";
  }

  public async confirm(title: string, body: string): Promise<boolean> {
    this.confirmations.push({ title, body });
    return this.answers.shift() ?? true;
  }
}
