/**
 * Provider-neutral presentation seams. Feature screens must depend on these
 * contracts, keeping UI libraries, graph engines and native APIs replaceable.
 */
export interface ComponentAdapter {
  readonly name: string;
}

export interface GraphAdapter {
  readonly name: string;
}

export interface CollectionAdapter {
  readonly name: string;
}

/**
 * The actions a user may choose for a path a conversation showed. They mirror
 * the core's own vocabulary; the webview never resolves or opens a path by
 * itself, so a menu is only a request the trusted core may refuse.
 */
export type PathActionName =
  | "inspect"
  | "open_default"
  | "open_editor"
  | "reveal";

/** One action request, scoped to the Chat whose frozen workspace owns the path. */
export interface PathActionRequest {
  readonly chatId: string;
  readonly path: string;
  readonly action: PathActionName;
}

/** The core's answer for one requested action. */
export interface PathActionOutcome {
  /** Resolved path inside the workspace, when it could be resolved at all. */
  readonly absolutePath: string;
  /** Whether the path is inside the workspace and exists. */
  readonly eligible: boolean;
  /** Stable explanation when the path is not eligible. */
  readonly reason?: string;
  /** Whether an action was actually performed. */
  readonly performed: boolean;
}

/** Runs one path action for the Chat the conversation on screen belongs to. */
export type RunPathAction = (
  path: string,
  action: PathActionName,
) => Promise<PathActionOutcome>;

export interface NativePresentationAdapter {
  readonly name: string;
  notify(title: string, body: string): Promise<void>;
  confirm(title: string, body: string): Promise<boolean>;
  message(title: string, body: string): Promise<void>;
  /** `extensions` narrows a file choice; an empty list offers everything. */
  pickFile(extensions?: readonly string[]): Promise<string | null>;
  pickFolder(): Promise<string | null>;
  /**
   * Acts on a path the conversation showed. The desktop host resolves it
   * inside the Chat's frozen workspace, so a refused or missing target comes
   * back as data instead of an exception.
   */
  pathAction(request: PathActionRequest): Promise<PathActionOutcome>;
}

export const nativePresentationEvent = "aworkit:native-presentation";
export type NativePresentationRequest =
  | {
      readonly kind: "notification";
      readonly title: string;
      readonly body: string;
    }
  | {
      readonly kind: "confirmation";
      readonly title: string;
      readonly body: string;
      readonly resolve: (accepted: boolean) => void;
    };

export interface DesktopAdapters {
  readonly components: ComponentAdapter;
  readonly graph: GraphAdapter;
  readonly collections: CollectionAdapter;
  readonly nativePresentation: NativePresentationAdapter;
}
