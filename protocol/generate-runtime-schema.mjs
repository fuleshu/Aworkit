import { readFile, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";

const source = new URL("./schema/aworkit-envelope.v1.schema.json", import.meta.url);
const target = new URL("../desktop/src/protocol/generated-schema-info.ts", import.meta.url);
const schemaBytes = await readFile(source);
const schema = JSON.parse(schemaBytes.toString("utf8"));
const alternatives = schema.oneOf;
const first = alternatives[0];
const versionFromAlternative = first.properties.schemaVersion.const;
const kinds = alternatives.map((alternative) => alternative.properties.kind.const);
const stableIdPattern = schema.$defs.stableId.pattern;
const maximumGeneration = schema.$defs.generation.maximum;
const schemaSha256 = createHash("sha256").update(schemaBytes).digest("hex");

if (!alternatives.every((alternative) => alternative.properties.schemaVersion.const === versionFromAlternative)) {
  throw new Error("all envelope alternatives must use one schema version");
}

const quotedKinds = kinds.map((kind) => `  ${JSON.stringify(kind)},`).join("\n");
const output = `/** Generated from protocol/schema/aworkit-envelope.v1.schema.json. */
import envelopeSchema from "../../../protocol/schema/aworkit-envelope.v1.schema.json";

export const AWORKIT_ENVELOPE_SCHEMA_ID =
  ${JSON.stringify(schema.$id)} as const;
export const AWORKIT_ENVELOPE_SCHEMA_VERSION = ${versionFromAlternative} as const;
export const AWORKIT_ENVELOPE_SCHEMA_SHA256 =
  ${JSON.stringify(schemaSha256)} as const;
export const AWORKIT_STABLE_ID_PATTERN = ${JSON.stringify(stableIdPattern)} as const;
export const AWORKIT_MAX_SAFE_WIRE_INTEGER = ${maximumGeneration} as const;
export const AWORKIT_ENVELOPE_KINDS = [
${quotedKinds}
] as const;
export const AWORKIT_ENVELOPE_SCHEMA = envelopeSchema;
`;

if (process.argv.includes("--check")) {
  const existing = await readFile(target, "utf8");
  if (existing !== output) {
    throw new Error("generated runtime schema is stale; run the protocol generator");
  }
} else {
  await writeFile(target, output);
}
