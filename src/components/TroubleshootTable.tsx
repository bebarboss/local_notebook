import { troubleshootRows } from "../i18n/th";

export function TroubleshootTable() {
  return (
    <div className="list-card" role="table" aria-label="วิธีแก้ปัญหา">
      <div className="trouble-row" role="row" style={{ background: "var(--sidebar)", fontSize: 12, color: "var(--muted)" }}>
        <span className="s" role="columnheader">อาการ</span>
        <span role="columnheader">ตรวจสอบ</span>
      </div>
      {troubleshootRows.map((r) => (
        <div className="trouble-row" role="row" key={r.symptom}>
          <span className={`s${r.mono ? " mono" : ""}`} role="cell">{r.symptom}</span>
          <span className="c" role="cell">{r.check}</span>
        </div>
      ))}
    </div>
  );
}
