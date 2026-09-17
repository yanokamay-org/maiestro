// The "Remove {name} from mAIestro Code?" confirmation block, shared by the repo and
// identity removals in both windows.
export function RemoveConfirm({ name, body, onRemove, onCancel }: {
  name: string;
  body: string;
  onRemove: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="cleanup-confirm">
      <p className="cleanup-lead">Remove {name} from mAIestro Code?</p>
      <p className="cleanup-confirm-body">{body}</p>
      <div className="issue-actions">
        <button className="btn-danger" onClick={onRemove}>Remove</button>
        <button className="btn-ghost" onClick={onCancel}>Cancel</button>
      </div>
    </div>
  );
}
