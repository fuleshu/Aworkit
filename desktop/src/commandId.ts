export type CommandScope =
  | "chat"
  | "settings"
  | "workflow"
  | "management";

/**
 * Creates a durable idempotency key that remains collision-resistant across
 * webview reloads and application restarts. There is deliberately no
 * timestamp/counter fallback: without Web Crypto, privileged commands fail
 * closed instead of risking reuse of an earlier durable command ID.
 */
export function createDurableCommandId(scope: CommandScope): string {
  const crypto = globalThis.crypto;
  if (crypto === undefined)
    throw new Error("secure command-ID generation is unavailable");
  const nonce =
    typeof crypto.randomUUID === "function"
      ? crypto.randomUUID()
      : randomHex(crypto);
  return `desktop.${scope}.${nonce}`;
}

function randomHex(crypto: Crypto): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (value) => value.toString(16).padStart(2, "0")).join(
    "",
  );
}

/**
 * Creates an id for a locally created collection record (a project, an MCP
 * server, …).
 *
 * Unlike a durable command id this may fall back to a weaker source: such a
 * record only has to be unique inside the settings document that holds it, and
 * a missing Web Crypto must not stop the user from adding one.
 */
export function createLocalId(scope: string): string {
  const random = globalThis.crypto?.randomUUID?.().replaceAll("-", "");
  return `${scope}.${random ?? `${Date.now()}${Math.random().toString(16).slice(2)}`}`;
}
