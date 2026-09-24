import type { RuntimeEvent } from "./corePort";

/** One entry of the agent's task list. */
export interface ChatTodo {
  readonly content: string;
  readonly status: "pending" | "in_progress" | "completed";
}

/** Completed and total counts for the list summary. */
export interface TodoProgress {
  readonly done: number;
  readonly total: number;
}

/**
 * The task list the agent last wrote, or null when no list exists yet.
 *
 * Every reader of a task list — the timeline card and the composer control —
 * goes through {@link todosFromFact}, so the two can never disagree about an
 * entry or its status.
 */
export function todosFromFact(fact: unknown): readonly ChatTodo[] {
  const todos = record(fact).todos;
  if (!Array.isArray(todos)) return [];
  return todos.map((todo) => {
    const entry = record(todo);
    return {
      content:
        typeof entry.content === "string"
          ? entry.content
          : String(entry.content ?? ""),
      status: statusOf(entry.status),
    };
  });
}

/**
 * Newest task list in the committed stream, or null when the agent has not
 * written one. Later facts supersede earlier ones, so only the highest sequence
 * decides; the scan is one pass and copies nothing.
 */
export function currentTodos(
  events: readonly RuntimeEvent[],
): readonly ChatTodo[] | null {
  let latest: {
    readonly sequence: number;
    readonly todos: readonly ChatTodo[];
  } | null = null;
  for (const event of events) {
    if (event.kind !== "tool.todo") continue;
    if (latest === null || event.sequence >= latest.sequence) {
      latest = {
        sequence: event.sequence,
        todos: todosFromFact(event.payload),
      };
    }
  }
  return latest === null ? null : latest.todos;
}

/** Completed and total counts, used for the control's summary and tooltip. */
export function todoProgress(todos: readonly ChatTodo[]): TodoProgress {
  let done = 0;
  for (const todo of todos) if (todo.status === "completed") done += 1;
  return { done, total: todos.length };
}

/** Human label for one status, shared by the control and its tests. */
export function todoStatusLabel(status: ChatTodo["status"]): string {
  return status === "completed"
    ? "Completed"
    : status === "in_progress"
      ? "In progress"
      : "Pending";
}

/**
 * Normalizes the status a writer may spell differently. An unknown or missing
 * status counts as pending, which is the only safe reading of unfinished work.
 */
function statusOf(value: unknown): ChatTodo["status"] {
  const text = typeof value === "string" ? value : "";
  if (text === "completed" || text === "done") return "completed";
  if (text === "in_progress" || text === "active") return "in_progress";
  return "pending";
}

function record(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}
