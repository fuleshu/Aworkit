/** Generated from protocol/schema/aworkit-envelope.v1.schema.json. */
import envelopeSchema from "../../../protocol/schema/aworkit-envelope.v1.schema.json";

export const AWORKIT_ENVELOPE_SCHEMA_ID =
  "https://aworkit.local/schema/aworkit-envelope.v1.schema.json" as const;
export const AWORKIT_ENVELOPE_SCHEMA_VERSION = 1 as const;
export const AWORKIT_ENVELOPE_SCHEMA_SHA256 =
  "108d534502abf9fe975d85bf6b3678f839f5c3a7536e4caa27e07033444bdec8" as const;
export const AWORKIT_STABLE_ID_PATTERN = "^[A-Za-z0-9._-]{1,128}$" as const;
export const AWORKIT_MAX_SAFE_WIRE_INTEGER = 9007199254740991 as const;
export const AWORKIT_ENVELOPE_KINDS = [
  "command",
  "event",
  "request",
  "result",
  "error",
] as const;
export const AWORKIT_ENVELOPE_SCHEMA = envelopeSchema;
