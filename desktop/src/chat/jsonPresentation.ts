/** One formatting policy for JSON shown throughout Chat presentation. */
export function prettyJson(
  value: unknown,
  unavailableMessage = "The data is not serializable.",
): string {
  try {
    return JSON.stringify(value, null, 2) ?? "null";
  } catch {
    return unavailableMessage;
  }
}
