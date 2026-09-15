import { useCallback, useMemo, useSyncExternalStore } from "react";
import type { ChatIntent } from "./types";
import type { ComposerState } from "./composer";

interface Draft {
  readonly state: ComposerState;
  readonly retryIntent: ChatIntent | null;
  readonly submitting: boolean;
}

/** Workspace-local drafts survive Chat navigation. Late receipts update only
 * their original Chat, including when that Chat has since been remounted. */
export class ComposerDrafts {
  private readonly entries = new Map<string, Draft>();
  private readonly listeners = new Set<() => void>();
  get(id: string, initial: ComposerState): Draft {
    let draft = this.entries.get(id);
    if (!draft) {
      draft = { state: initial, retryIntent: null, submitting: false };
      this.entries.set(id, draft);
    }
    return draft;
  }
  update(id: string, change: (draft: Draft) => Draft): void {
    const current = this.entries.get(id);
    if (!current) return;
    this.entries.set(id, change(current));
    this.listeners.forEach(listener => listener());
  }
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };
}

export function useComposerDraft(id: string, initial: ComposerState, provided?: ComposerDrafts) {
  const local = useMemo(() => new ComposerDrafts(), []);
  const store = provided ?? local;
  const read = useCallback(() => store.get(id, initial), [store, id, initial]);
  const draft = useSyncExternalStore(store.subscribe, read);
  return {
    ...draft,
    setState: (change: (state: ComposerState) => ComposerState) => store.update(id, current => ({ ...current, state: change(current.state) })),
    setRetryIntent: (retryIntent: ChatIntent | null) => store.update(id, current => ({ ...current, retryIntent })),
    setSubmitting: (submitting: boolean) => store.update(id, current => ({ ...current, submitting })),
  };
}
