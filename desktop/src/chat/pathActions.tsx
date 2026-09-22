import { createContext, useContext, type ReactNode } from "react";
import type { RunPathAction } from "../adapters/contracts";

export type {
  PathActionName,
  PathActionOutcome,
  PathActionRequest,
  RunPathAction,
} from "../adapters/contracts";

const PathActionContext = createContext<RunPathAction | null>(null);

/**
 * Supplies path actions to the conversation. It is absent in a browser preview
 * and where no Chat owns the rendered path, in which case a path stays inert.
 */
export function PathActionProvider({
  run,
  children,
}: {
  readonly run: RunPathAction | undefined;
  readonly children: ReactNode;
}): React.JSX.Element {
  return (
    <PathActionContext.Provider value={run ?? null}>
      {children}
    </PathActionContext.Provider>
  );
}

/** The path actions available to the conversation rendered right now. */
export function usePathAction(): RunPathAction | null {
  return useContext(PathActionContext);
}
