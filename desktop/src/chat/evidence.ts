import type { EvidenceRecord } from "./types";

export interface EvidenceQuery { readonly filter?: EvidenceRecord["category"]; readonly offset: number; readonly limit: number; }
export interface EvidencePage { readonly total: number; readonly items: readonly EvidenceRecord[]; }

/** Projects evidence facts without filling in missing, redacted, opaque, or expired details. */
export function queryEvidence(records: readonly EvidenceRecord[], query: EvidenceQuery): EvidencePage {
  const filtered = query.filter === undefined ? records : records.filter((record) => record.category === query.filter);
  return { total: filtered.length, items: filtered.slice(Math.max(0, query.offset), Math.max(0, query.offset) + Math.max(1, query.limit)) };
}

export function inspectEvidence(record: EvidenceRecord): string {
  if (record.state !== "available") return `Evidence is ${record.state}. No additional value is inferred.`;
  return JSON.stringify(record.value, null, 2);
}
