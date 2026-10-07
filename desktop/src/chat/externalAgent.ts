/**
 * The delegated external agents Aworkit can drive.
 *
 * Each activity note the host commits carries the product identity it came
 * from, so the Chat can attribute the note to the delegated agent by name
 * instead of showing it as the main model.
 */
const EXTERNAL_AGENT_LABELS: Readonly<Record<string, string>> = {
  codex: "Codex",
  claude: "Claude Code",
  claude_code: "Claude Code",
};

/** The display name of a delegated agent, or undefined for an unknown product. */
export function externalAgentLabel(product: unknown): string | undefined {
  if (typeof product !== "string" || product.length === 0) return undefined;
  return EXTERNAL_AGENT_LABELS[product];
}
