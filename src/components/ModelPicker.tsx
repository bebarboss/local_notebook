import { ChevronDown } from "lucide-react";
import type { Effort, ModelConfig } from "../lib/types";
import { usePopover } from "./ui";

const EFFORTS: Effort[] = ["low", "medium", "high"];

export function ModelPicker({
  models, model, effort, onChange, disabled, up = true,
}: {
  models: ModelConfig[]; model: string; effort: Effort; onChange: (model: string, effort: Effort) => void; disabled?: boolean; up?: boolean;
}) {
  const pop = usePopover();
  const current = models.find((m) => m.id === model);
  const efforts = current?.supportsReasoningEffort?.length ? current.supportsReasoningEffort : EFFORTS;
  return (
    <div ref={pop.ref} style={{ position: "relative" }}>
      <button
        type="button"
        className="btn btn-ghost"
        style={{ fontWeight: 500, padding: "0 10px", color: "var(--text-2)" }}
        aria-haspopup="dialog"
        aria-expanded={pop.open}
        aria-label={`โมเดล ${model} ระดับการคิด ${effort}`}
        disabled={disabled}
        onClick={() => pop.setOpen(!pop.open)}
      >
        <span className="mono" style={{ fontSize: 13 }}>{model || "เลือกโมเดล"}</span>
        <span className="muted">· {effort}</span>
        <ChevronDown size={14} strokeWidth={2} aria-hidden="true" className="muted" />
      </button>
      {pop.open && (
        <div className="popover" role="dialog" aria-label="เลือกโมเดล" style={{ right: 0, [up ? "bottom" : "top"]: 42, width: 240 }}>
          <div className="menu-label">โมเดล</div>
          <div role="radiogroup" aria-label="โมเดล">
            {models.length === 0 && <div style={{ padding: "6px 10px", fontSize: 12, color: "var(--muted)" }}>ไม่มีโมเดลที่ใช้ได้ — ทดสอบโมเดลใน ตั้งค่า</div>}
            {models.map((m) => (
              <button key={m.id} role="radio" aria-checked={m.id === model} className="menu-item" onClick={() => onChange(m.id, effort)}>
                <span className="mono">{m.name}</span>
              </button>
            ))}
          </div>
          <div className="menu-sep" />
          <div className="menu-label">Reasoning effort</div>
          <div className="segmented" role="radiogroup" aria-label="Reasoning effort" style={{ margin: "4px 6px 6px", display: "flex" }}>
            {EFFORTS.map((e) => (
              <button key={e} role="radio" aria-checked={e === effort} disabled={!efforts.includes(e)} style={{ flex: 1 }} onClick={() => onChange(model, e)}>{e}</button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
