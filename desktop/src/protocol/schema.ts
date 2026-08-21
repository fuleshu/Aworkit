import { z } from "zod";
import { AWORKIT_ENVELOPE_SCHEMA_VERSION } from "./generated-schema-info";

/** Runtime schema for the shared Aworkit V1 process-boundary envelope. */
export const stableIdSchema = z.string().regex(/^[A-Za-z0-9._-]{1,128}$/);
export const schemaVersionSchema = z.literal(AWORKIT_ENVELOPE_SCHEMA_VERSION);
export const generationSchema = z.number().int().nonnegative();
export const envelopeKindSchema = z.enum(["command", "event", "request", "result", "error"]);

/** Builds a strict envelope parser while domain modules retain their own DTOs. */
export function envelopeSchema<TPayload extends z.ZodType>(payload: TPayload) {
  return z.object({
    schemaVersion: schemaVersionSchema,
    messageId: stableIdSchema,
    generation: generationSchema,
    kind: envelopeKindSchema,
    payload,
  }).strict();
}

/** The base command is the shared golden-fixture payload. */
export const baseCommandSchema = z.object({
  name: z.string().min(1),
  targetId: stableIdSchema,
}).strict();

export const commandEnvelopeSchema = envelopeSchema(baseCommandSchema).refine(
  (envelope) => envelope.kind === "command",
  { message: "command payload requires a command envelope" },
);

export type CommandEnvelope = z.infer<typeof commandEnvelopeSchema>;
