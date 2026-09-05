import { useCallback, useEffect, useRef, useState } from "react";
import type { SettingsLeaveGuard } from "../../shell/settingsNavigation";

interface LeaveState {
  readonly dirty: boolean;
  readonly busy: boolean;
  readonly mutationVerified: boolean;
  readonly save: () => Promise<boolean>;
  readonly discard: (confirm?: boolean) => Promise<boolean>;
}

/** Defers navigation until the existing Settings mutation proof and leave decision settle. */
export function useSettingsLeave(state: LeaveState, register?: (guard: SettingsLeaveGuard | null) => void) {
  const latest = useRef(state);
  latest.current = state;
  const pending = useRef<(() => void) | null>(null);
  const waitingForMutation = useRef(false);
  const [prompt, setPrompt] = useState(false);
  const [deciding, setDeciding] = useState(false);
  const request = useCallback<SettingsLeaveGuard>(leave => {
    pending.current = leave;
    if (latest.current.busy) { waitingForMutation.current = true; return; }
    if (latest.current.dirty) setPrompt(true);
    else { pending.current = null; leave(); }
  }, []);
  useEffect(() => { register?.(request); return () => register?.(null); }, [register, request]);
  useEffect(() => {
    if (state.busy || !waitingForMutation.current) return;
    waitingForMutation.current = false;
    const leave = pending.current;
    if (!state.mutationVerified) { pending.current = null; return; }
    if (leave) request(leave);
  }, [state.busy, state.mutationVerified, request]);
  const stay = () => { pending.current = null; setPrompt(false); };
  const decide = async (kind: "save" | "discard") => {
    if (deciding || latest.current.busy) return;
    setDeciding(true);
    try {
      const verified = await (kind === "save" ? latest.current.save() : latest.current.discard(false));
      if (!verified) return;
      const leave = pending.current;
      pending.current = null;
      setPrompt(false);
      leave?.();
    } finally { setDeciding(false); }
  };
  return { prompt, deciding, stay, decide };
}
