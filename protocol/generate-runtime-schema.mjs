import { readFile, writeFile } from "node:fs/promises";

const source = new URL("./schema/aworkit-envelope.v1.schema.json", import.meta.url);
const target = new URL("../desktop/src/protocol/generated-schema-info.ts", import.meta.url);
const schema = JSON.parse(await readFile(source, "utf8"));
const version = schema.properties.schemaVersion.const;

await writeFile(
  target,
  `/** Generated from protocol/schema/aworkit-envelope.v1.schema.json. */\nexport const AWORKIT_ENVELOPE_SCHEMA_ID = ${JSON.stringify(schema.$id)} as const;\nexport const AWORKIT_ENVELOPE_SCHEMA_VERSION = ${version} as const;\n`,
);
