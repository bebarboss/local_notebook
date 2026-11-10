// Small shared primitives: Modal, ConfirmDialog, Switch, Popover hook, Skeleton, ErrorState.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { th } from "../i18n/th";

export function Modal({ title, onClose, children, width }: { title: string; onClose: () => void; children: ReactNode; width?: number }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null;
    const first = ref.current?.querySelector<HTMLElement>("input, textarea, select, button");
    first?.focus();
    return () => prev?.focus();
  }, []);
  // portal: modals can be opened from inline content (e.g. a citation inside <p>)
  return createPortal(
    <div
      className="modal-backdrop"
      onMouseDown={(e) => e.target === e.currentTarget && onClose()}
      onKeyDown={(e) => e.key === "Escape" && onClose()}
    >
      <div className="modal" role="dialog" aria-modal="true" aria-label={title} ref={ref} style={width ? { maxWidth: width } : undefined}>
        <h2>{title}</h2>
        {children}
      </div>
    </div>,
    document.body,
  );
}

export function ConfirmDialog({
  title, body, confirmLabel, danger, onConfirm, onCancel, extra,
}: {
  title: string; body: ReactNode; confirmLabel: string; danger?: boolean;
  onConfirm: () => void | Promise<void>; onCancel: () => void; extra?: ReactNode;
}) {
  const [busy, setBusy] = useState(false);
  return (
    <Modal title={title} onClose={onCancel}>
      <div style={{ color: "var(--text-2)", lineHeight: 1.7 }}>{body}</div>
      {extra}
      <div className="modal-actions">
        <button className="btn btn-lg" onClick={onCancel}>{th.cancel}</button>
        <button
          className={`btn btn-lg ${danger ? "btn-danger-solid" : "btn-primary"}`}
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            try { await onConfirm(); } finally { setBusy(false); }
          }}
        >
          {confirmLabel}
        </button>
      </div>
    </Modal>
  );
}

export function Switch({ checked, onChange, label }: { checked: boolean; onChange: (v: boolean) => void; label: string }) {
  return (
    <label className="switch">
      <input type="checkbox" role="switch" checked={checked} aria-label={label} onChange={(e) => onChange(e.target.checked)} />
      <span />
    </label>
  );
}

/** Closes on outside click / Escape. */
export function usePopover() {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => { if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false); };
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") setOpen(false); };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => { document.removeEventListener("mousedown", onDown); document.removeEventListener("keydown", onKey); };
  }, [open]);
  return { open, setOpen, ref };
}

export function Skeleton({ h = 14, w = "100%", style }: { h?: number; w?: number | string; style?: React.CSSProperties }) {
  return <div className="skeleton" style={{ height: h, width: w, ...style }} aria-hidden="true" />;
}

export function SkeletonList({ rows = 3, h = 52 }: { rows?: number; h?: number }) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }} aria-busy="true" aria-label="กำลังโหลด">
      {Array.from({ length: rows }, (_, i) => <Skeleton key={i} h={h} style={{ borderRadius: 12 }} />)}
    </div>
  );
}

export function ErrorState({ message, onRetry }: { message: string; onRetry?: () => void }) {
  return (
    <div className="msg-error" role="alert">
      <span>{message}</span>
      {onRetry && <button className="btn" onClick={onRetry}>{th.tryAgain}</button>}
    </div>
  );
}

export function CodeBadge({ code, color, size = 32 }: { code: string; color: string; size?: number }) {
  return (
    <span className="code-badge" style={{ background: color, width: size, height: size, borderRadius: size * 0.28, fontSize: code.length > 2 ? 11 : 12 }} aria-hidden="true">
      {code}
    </span>
  );
}

export function Logo({ size = 17 }: { size?: number }) {
  return (
    <div className="logo" style={{ fontSize: size }}>
      <div className="logo-mark" aria-hidden="true">D</div>
      <span>DocAsk</span>
    </div>
  );
}
