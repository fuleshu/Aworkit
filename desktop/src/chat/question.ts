/**
 * The typed view of one model question.
 *
 * A question arrives as a committed fact, so the renderer parses the durable
 * payload instead of keeping its own copy. What the user is allowed to answer
 * is decided by the core against the same durable question; this module only
 * shapes the draft and decides whether it is complete enough to send.
 */

export type QuestionKind = "choice" | "file" | "folder";

export interface QuestionOption {
  readonly id: string;
  readonly label: string;
  readonly description?: string;
}

export interface QuestionModel {
  readonly questionId: string;
  readonly kind: QuestionKind;
  readonly prompt: string;
  readonly title?: string;
  readonly options: readonly QuestionOption[];
  readonly allowFreeText: boolean;
  readonly defaultOptionId?: string;
  /** Extensions a file question asked the chooser to show. */
  readonly extensions: readonly string[];
}

/** What the user chose in the dialog. `cancelled` means they skipped it. */
export interface QuestionAnswerInput {
  readonly optionId?: string;
  readonly freeText?: string;
  readonly path?: string;
  readonly cancelled?: boolean;
}

export interface QuestionDraft {
  readonly optionId: string | null;
  readonly freeText: string;
  readonly path: string | null;
}

export const EMPTY_QUESTION_DRAFT: QuestionDraft = {
  optionId: null,
  freeText: "",
  path: null,
};

function record(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function text(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

/** Parses one committed `question.asked` payload into its typed view. */
export function questionFromMetadata(
  metadata: unknown,
  fallbackId: string,
): QuestionModel | undefined {
  const fact = record(metadata);
  const kind = text(fact.kind);
  if (kind !== "choice" && kind !== "file" && kind !== "folder") return undefined;
  const prompt = text(fact.prompt);
  if (prompt === undefined) return undefined;
  const options = Array.isArray(fact.options)
    ? fact.options.flatMap((candidate): QuestionOption[] => {
        const option = record(candidate);
        const id = text(option.id);
        const label = text(option.label);
        if (id === undefined || label === undefined) return [];
        return [
          {
            id,
            label,
            ...(text(option.description) === undefined
              ? {}
              : { description: text(option.description)! }),
          },
        ];
      })
    : [];
  return {
    questionId: text(fact.questionId) ?? fallbackId,
    kind,
    prompt,
    ...(text(fact.title) === undefined ? {} : { title: text(fact.title)! }),
    options,
    allowFreeText: fact.allowFreeText === true,
    ...(text(fact.defaultOptionId) === undefined
      ? {}
      : { defaultOptionId: text(fact.defaultOptionId)! }),
    extensions: Array.isArray(fact.extensions)
      ? fact.extensions.filter(
          (value): value is string =>
            typeof value === "string" && value.length > 0,
        )
      : [],
  };
}

/** The opening draft for a question: its declared default, when there is one. */
export function initialQuestionDraft(question: QuestionModel): QuestionDraft {
  return {
    ...EMPTY_QUESTION_DRAFT,
    optionId: question.defaultOptionId ?? null,
  };
}

/** Whether the draft carries enough for the core to accept the answer. */
export function questionDraftComplete(
  question: QuestionModel,
  draft: QuestionDraft,
): boolean {
  if (question.kind === "choice") {
    if (draft.optionId !== null) return true;
    return question.allowFreeText && draft.freeText.trim().length > 0;
  }
  return draft.path !== null && draft.path.length > 0;
}

/** Shapes the draft into the exact command payload. */
export function questionAnswerInput(
  question: QuestionModel,
  draft: QuestionDraft,
): QuestionAnswerInput {
  if (question.kind === "choice") {
    return {
      ...(draft.optionId === null ? {} : { optionId: draft.optionId }),
      ...(question.allowFreeText && draft.freeText.trim().length > 0
        ? { freeText: draft.freeText.trim() }
        : {}),
    };
  }
  return { path: draft.path ?? "" };
}

/** The label shown for one question kind. */
export function questionKindLabel(kind: QuestionKind): string {
  if (kind === "file") return "Choose a file";
  if (kind === "folder") return "Choose a folder";
  return "Choose an answer";
}
