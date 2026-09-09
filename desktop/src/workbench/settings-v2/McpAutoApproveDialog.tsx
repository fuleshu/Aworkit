import { useEffect, useRef } from "react";
import "./mcp-auto-approve.css";

/** A local confirmation keeps the MCP choice attached to the Settings draft. */
export function McpAutoApproveDialog({ tool, server, onDecision }: {
  readonly tool: string;
  readonly server: string;
  readonly onDecision: (accepted: boolean) => void;
}): React.JSX.Element {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialog.current?.showModal();
    return () => { if (previous?.isConnected) previous.focus({ preventScroll: true }); };
  }, []);
  return <dialog ref={dialog} className="workbench-dialog mcp-auto-approve-dialog"
    aria-labelledby="mcp-auto-approve-title" aria-describedby="mcp-auto-approve-risk"
    onCancel={event => { event.preventDefault(); onDecision(false); }}>
    <h2 id="mcp-auto-approve-title">Enable Auto approve?</h2>
    <p id="mcp-auto-approve-risk">Allow <strong>{tool}</strong> on <strong>{server}</strong> to run without approval prompts or automatic review, even if it changes or deletes data. You are trusting this MCP server to act with its available permissions.</p>
    <p>This setting applies to new Chats after saving.</p>
    <div className="settings-leave-actions">
      <button type="button" autoFocus title="Keep Auto approve off" onClick={() => onDecision(false)}>Cancel</button>
      <button type="button" className="primary-action" title="Allow this MCP tool to run without approval" onClick={() => onDecision(true)}>Confirm</button>
    </div>
  </dialog>;
}
