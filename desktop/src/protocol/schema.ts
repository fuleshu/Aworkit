import { z } from "zod";
import {
  AWORKIT_ENVELOPE_KINDS,
  AWORKIT_ENVELOPE_SCHEMA,
  AWORKIT_ENVELOPE_SCHEMA_VERSION,
} from "./generated-schema-info";

type SchemaNode = {
  readonly $ref?: string;
  readonly const?: string | number | boolean | null;
  readonly type?: "object" | "string" | "integer" | "boolean";
  readonly pattern?: string;
  readonly minLength?: number;
  readonly maxLength?: number;
  readonly minimum?: number;
  readonly maximum?: number;
  readonly additionalProperties?: boolean;
  readonly required?: readonly string[];
  readonly properties?: Readonly<Record<string, SchemaNode>>;
};

const definitions = AWORKIT_ENVELOPE_SCHEMA.$defs as Readonly<
  Record<string, SchemaNode>
>;

/** Compiles the checked-in canonical JSON Schema subset into strict Zod parsers. */
function compileSchema(node: SchemaNode): z.ZodTypeAny {
  if (node.$ref !== undefined) {
    const prefix = "#/$defs/";
    if (!node.$ref.startsWith(prefix))
      throw new Error("unsupported schema ref");
    const definition = definitions[node.$ref.slice(prefix.length)];
    if (definition === undefined) throw new Error("unknown schema ref");
    return compileSchema(definition);
  }
  if (node.const !== undefined) return z.literal(node.const);
  if (node.type === "string") {
    let schema = z.string();
    if (node.pattern !== undefined)
      schema = schema.refine(
        (value: string) => new RegExp(node.pattern as string).test(value),
        "string does not match canonical pattern",
      );
    if (node.minLength !== undefined)
      schema = schema.refine(
        (value: string) =>
          Array.from(value).length >= (node.minLength as number),
        "string is shorter than canonical bound",
      );
    if (node.maxLength !== undefined)
      schema = schema.refine(
        (value: string) =>
          Array.from(value).length <= (node.maxLength as number),
        "string exceeds canonical bound",
      );
    return schema;
  }
  if (node.type === "integer") {
    let schema = z.number().int();
    if (node.minimum !== undefined) schema = schema.min(node.minimum);
    if (node.maximum !== undefined) schema = schema.max(node.maximum);
    return schema;
  }
  if (node.type === "boolean") return z.boolean();
  if (node.type === "object" && node.properties !== undefined) {
    const required = new Set(node.required ?? []);
    const shape: Record<string, z.ZodTypeAny> = {};
    for (const [name, property] of Object.entries(node.properties)) {
      const compiled = compileSchema(property);
      shape[name] = required.has(name) ? compiled : compiled.optional();
    }
    const object = z.object(shape);
    return node.additionalProperties === false ? object.strict() : object;
  }
  throw new Error("canonical schema uses an unsupported construct");
}

export const stableIdSchema = compileSchema(definitions.stableId);
export const schemaVersionSchema = z.literal(AWORKIT_ENVELOPE_SCHEMA_VERSION);
export const generationSchema = compileSchema(definitions.generation);
export const envelopeKindSchema = z.enum(AWORKIT_ENVELOPE_KINDS);

export function envelopeSchema<
  TKind extends (typeof AWORKIT_ENVELOPE_KINDS)[number],
  TPayload extends z.ZodType,
>(kind: TKind, payload: TPayload) {
  return z
    .object({
      schemaVersion: schemaVersionSchema,
      messageId: stableIdSchema,
      generation: generationSchema,
      kind: z.literal(kind),
      payload,
    })
    .strict();
}

export const baseCommandSchema = compileSchema(definitions.baseCommand);
export const baseEventSchema = compileSchema(definitions.baseEvent);
export const baseRequestSchema = compileSchema(definitions.baseRequest);
export const baseResultSchema = compileSchema(definitions.baseResult);
export const baseErrorSchema = compileSchema(definitions.baseError);

const alternatives = AWORKIT_ENVELOPE_SCHEMA.oneOf.map((alternative) =>
  compileSchema(alternative as SchemaNode),
) as [z.ZodTypeAny, z.ZodTypeAny, ...z.ZodTypeAny[]];

const schemasByKind = Object.fromEntries(
  AWORKIT_ENVELOPE_SCHEMA.oneOf.map((alternative, index) => [
    alternative.properties.kind.const,
    alternatives[index],
  ]),
) as Record<(typeof AWORKIT_ENVELOPE_KINDS)[number], z.ZodTypeAny>;

export const commandEnvelopeSchema = schemasByKind.command;
export const eventEnvelopeSchema = schemasByKind.event;
export const requestEnvelopeSchema = schemasByKind.request;
export const resultEnvelopeSchema = schemasByKind.result;
export const errorEnvelopeSchema = schemasByKind.error;
export const baseEnvelopeSchema = z.union(alternatives);

export type BaseEnvelope = {
  schemaVersion: typeof AWORKIT_ENVELOPE_SCHEMA_VERSION;
  messageId: string;
  generation: number;
  kind: (typeof AWORKIT_ENVELOPE_KINDS)[number];
  payload: unknown;
};
export type CommandEnvelope = BaseEnvelope & { kind: "command" };
