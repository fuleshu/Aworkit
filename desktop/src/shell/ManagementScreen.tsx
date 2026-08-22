export function ManagementScreen(): React.JSX.Element {
  return (
    <section className="management-screen">
      <header className="surface-toolbar">
        <div>
          <p className="eyebrow">PINNED</p>
          <h1>Management Chat</h1>
        </div>
        <span className="status ready">No active repair</span>
      </header>
      <div className="management-content">
        <section>
          <h2>Application health</h2>
          <dl>
            <div>
              <dt>Trusted core</dt>
              <dd>Connected</dd>
            </div>
            <div>
              <dt>Capability host</dt>
              <dd>Healthy</dd>
            </div>
            <div>
              <dt>Recurring errors</dt>
              <dd>0 open</dd>
            </div>
          </dl>
        </section>
        <section>
          <h2>Repair candidates</h2>
          <p>
            No candidate is awaiting review. Repair investigation and activation
            always require explicit, version-bound approval.
          </p>
          <button
            disabled
            title="No repair candidate is available"
            type="button"
          >
            Review candidate
          </button>
        </section>
      </div>
    </section>
  );
}
