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

export interface NativePresentationAdapter {
  readonly name: string;
  notify(title: string, body: string): Promise<void>;
}

export interface DesktopAdapters {
  readonly components: ComponentAdapter;
  readonly graph: GraphAdapter;
  readonly collections: CollectionAdapter;
  readonly nativePresentation: NativePresentationAdapter;
}
