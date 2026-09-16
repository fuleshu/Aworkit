/** Local activity indicator; the rest of the desktop remains interactive. */
export function ChatBusy({ label = "Loading recent activity…" }: { readonly label?: string }) {
  return <div className="chat-busy" role="status" aria-live="polite"><span className="chat-busy-spinner" aria-hidden="true" /><span>{label}</span></div>;
}
