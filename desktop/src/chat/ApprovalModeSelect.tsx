import { approvalModes, type ApprovalMode } from "./approvals";
import "./approvals.css";

/** The same three permissions modes in the composer and default Settings. */
export function ApprovalModeSelect({ value, disabled = false, onChange, label = "Approval mode" }: {
  readonly value: ApprovalMode; readonly disabled?: boolean;
  readonly onChange: (mode: ApprovalMode) => void; readonly label?: string;
}): React.JSX.Element {
  return <label className="approval-mode-select"><span>{label}</span>
    <select aria-label={label} title={approvalModes.find(mode => mode.value === value)?.description}
      value={value} disabled={disabled} onChange={event => onChange(event.target.value as ApprovalMode)}>
      {approvalModes.map(mode => <option key={mode.value} value={mode.value}>{mode.label}</option>)}
    </select>
  </label>;
}
