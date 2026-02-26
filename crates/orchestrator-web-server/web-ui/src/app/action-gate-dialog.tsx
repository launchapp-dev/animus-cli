import { useEffect, useRef, useState } from "react";

export type ActionGateConfig = {
  actionKey: string;
  targetId: string;
  confirmationPhrase: string;
  impactSummary: string;
  submitLabel: string;
};

type ActionGateDialogProps = {
  gate: ActionGateConfig | null;
  pending: boolean;
  errorMessage?: string | null;
  onConfirm: () => void;
  onClose: () => void;
};

export function matchesConfirmationPhrase(input: string, expected: string): boolean {
  return input.trim().replace(/\s+/g, " ").toUpperCase() === expected.toUpperCase();
}

export function ActionGateDialog(props: ActionGateDialogProps) {
  const [confirmationInput, setConfirmationInput] = useState("");
  const inputRef = useRef<HTMLInputElement | null>(null);
  const closeButtonRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    if (!props.gate) {
      setConfirmationInput("");
      return;
    }

    setConfirmationInput("");
    const handle = window.setTimeout(() => {
      inputRef.current?.focus();
    }, 0);

    return () => window.clearTimeout(handle);
  }, [props.gate]);

  useEffect(() => {
    if (props.gate) {
      return;
    }

    closeButtonRef.current?.focus();
  }, [props.gate]);

  if (!props.gate) {
    return null;
  }

  const hasRequiredMetadata =
    props.gate.actionKey.trim().length > 0 &&
    props.gate.targetId.trim().length > 0 &&
    props.gate.confirmationPhrase.trim().length > 0;

  const matchesPhrase = hasRequiredMetadata
    ? matchesConfirmationPhrase(confirmationInput, props.gate.confirmationPhrase)
    : false;
  const confirmDisabled = !matchesPhrase || props.pending;

  const failClosedMessage = hasRequiredMetadata
    ? null
    : "Confirmation metadata is incomplete, so this action is blocked.";

  return (
    <div className="gate-dialog-backdrop">
      <div
        className="gate-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="gate-dialog-title"
        aria-describedby="gate-dialog-description"
      >
        <h2 id="gate-dialog-title">Confirm High-Impact Action</h2>
        <p id="gate-dialog-description">{props.gate.impactSummary}</p>
        <p>
          <strong>Target:</strong> <code>{props.gate.targetId}</code>
        </p>
        <p>
          Type <code>{props.gate.confirmationPhrase}</code> to confirm.
        </p>

        <label>
          Confirmation phrase
          <input
            ref={inputRef}
            value={confirmationInput}
            onChange={(event) => setConfirmationInput(event.target.value)}
            placeholder={props.gate.confirmationPhrase}
            aria-invalid={confirmationInput.length > 0 && !matchesPhrase}
          />
        </label>

        {failClosedMessage ? (
          <p className="field-error" role="alert">
            {failClosedMessage}
          </p>
        ) : null}
        {props.errorMessage ? (
          <p className="field-error" role="alert">
            {props.errorMessage}
          </p>
        ) : null}

        <div className="panel-actions">
          <button type="button" className="danger-button" disabled={confirmDisabled} onClick={props.onConfirm}>
            {props.pending ? "Submitting..." : props.gate.submitLabel}
          </button>
          <button type="button" ref={closeButtonRef} onClick={props.onClose} disabled={props.pending}>
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}
