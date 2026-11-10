export function ProgressBar({ value, thin, label }: { value: number; thin?: boolean; label?: string }) {
  const pct = Math.max(0, Math.min(100, Math.round(value * 100)));
  return (
    <div className={`progress${thin ? " thin" : ""}`} role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} aria-label={label}>
      <div style={{ width: `${pct}%` }} />
    </div>
  );
}
