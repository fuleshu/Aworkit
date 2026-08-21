import { z } from "zod";

const stableId = z.string().regex(/^[A-Za-z0-9._-]{1,128}$/);
export const coreEventSchema = z.object({ sequence: z.number().int().nonnegative(), kind: z.string().min(1), payload: z.unknown() }).strict();
export const coreReceiptSchema = z.object({ commandId: stableId, accepted: z.boolean(), reason: z.string().optional() }).strict();
export type CoreEvent = z.infer<typeof coreEventSchema>;
export type CoreReceipt = z.infer<typeof coreReceiptSchema>;

export interface ProjectionState<T> { readonly sequence: number; readonly stale: boolean; readonly model: T; readonly pendingCommands: ReadonlySet<string>; }
export interface ProjectionReducer<T> { readonly initial: T; reduce(model: T, event: CoreEvent): T; }

/** Ordered, immutable core-event projection. Gaps require an explicit snapshot. */
export class ProjectionGateway<T> {
  private state: ProjectionState<T>;
  private nextCommand = 1;
  public constructor(private readonly reducer: ProjectionReducer<T>) { this.state = { sequence: 0, stale: false, model: reducer.initial, pendingCommands: new Set() }; }
  public snapshot(): ProjectionState<T> { return this.state; }
  public createCommandId(prefix = "desktop.command"): string { return `${prefix}.${this.nextCommand++}`; }
  public dispatch(commandId: string): ProjectionState<T> { this.state = { ...this.state, pendingCommands: new Set([...this.state.pendingCommands, commandId]) }; return this.state; }
  public receiveReceipt(input: unknown): ProjectionState<T> { const receipt = coreReceiptSchema.parse(input); const pending = new Set(this.state.pendingCommands); pending.delete(receipt.commandId); this.state = { ...this.state, pendingCommands: pending }; return this.state; }
  public receiveEvent(input: unknown): ProjectionState<T> { const event = coreEventSchema.parse(input); if (event.sequence <= this.state.sequence) return this.state; if (event.sequence !== this.state.sequence + 1) { this.state = { ...this.state, stale: true }; return this.state; } this.state = { ...this.state, sequence: event.sequence, model: this.reducer.reduce(this.state.model, event) }; return this.state; }
  public resynchronize(sequence: number, model: T): ProjectionState<T> { if (!Number.isSafeInteger(sequence) || sequence < this.state.sequence) throw new Error("projection snapshot is stale"); this.state = { ...this.state, sequence, model, stale: false }; return this.state; }
}
