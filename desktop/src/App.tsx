import type { DesktopAdapters } from "./adapters/contracts";

interface AppProps {
  readonly adapters: DesktopAdapters;
}

/** The deliberately small first-window placeholder for the Tauri shell. */
export function App({ adapters }: AppProps): React.JSX.Element {
  return (
    <main>
      <section aria-labelledby="aworkit-title">
        <p className="eyebrow">Aworkit desktop</p>
        <h1 id="aworkit-title">Milestone 01</h1>
        <p>
          The desktop presentation shell is ready. Feature screens will connect
          through Aworkit-owned contracts.
        </p>
        <dl>
          <div><dt>Components</dt><dd>{adapters.components.name}</dd></div>
          <div><dt>Graph</dt><dd>{adapters.graph.name}</dd></div>
          <div><dt>Collections</dt><dd>{adapters.collections.name}</dd></div>
          <div><dt>Native presentation</dt><dd>{adapters.nativePresentation.name}</dd></div>
        </dl>
      </section>
    </main>
  );
}
