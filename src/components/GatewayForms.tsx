// Pieces shared by the Setup Wizard and Settings: API key field, model list, test table.
import { useEffect, useState } from "react";
import { Check, Download, X } from "lucide-react";
import { api } from "../lib/api";
import type { ModelConfig, TestResult } from "../lib/types";
import { Switch } from "./ui";

export function keyFormatError(key: string): string | null {
  const k = key.trim();
  if (!k) return "กรุณาใส่ API Key";
  if (/\s/.test(k)) return "Key ต้องไม่มีช่องว่าง";
  if (!k.startsWith("sk-")) return "Key ต้องขึ้นต้นด้วย sk-";
  if (k.length < 12) return "Key สั้นเกินไป";
  return null;
}

export function ApiKeyField({ value, onChange, savedLast4 }: { value: string; onChange: (v: string) => void; savedLast4: string | null }) {
  const [show, setShow] = useState(false);
  const err = value ? keyFormatError(value) : null;
  return (
    <div className="field">
      <label htmlFor="apikey">API Key</label>
      <div style={{ display: "flex", gap: 8 }}>
        <input
          id="apikey"
          className="input mono"
          type={show ? "text" : "password"}
          autoComplete="off"
          spellCheck={false}
          value={value}
          placeholder={savedLast4 ? `sk-•••••••• ${savedLast4} (บันทึกแล้ว)` : "sk-..."}
          onChange={(e) => onChange(e.target.value)}
          onCopy={(e) => e.preventDefault()}
          onCut={(e) => e.preventDefault()}
          style={{ minHeight: 44, borderRadius: 12, flex: 1, minWidth: 0 }}
          aria-invalid={!!err}
          aria-describedby="apikey-hint"
        />
        <button type="button" className="btn btn-lg" style={{ fontWeight: 500 }} onClick={() => setShow((v) => !v)} aria-pressed={show}>
          {show ? "ซ่อน" : "แสดง"}
        </button>
      </div>
      <span id="apikey-hint" style={{ fontSize: 12 }} className={value && !err ? "ok-text" : undefined}>
        {!value ? <span className="muted">{savedLast4 ? "เว้นว่างไว้เพื่อใช้ Key เดิม" : "รูปแบบ: ขึ้นต้นด้วย sk- และไม่มีช่องว่าง"}</span> : err ? <span style={{ color: "var(--err-text)" }}>{err}</span> : "✓ รูปแบบถูกต้อง"}
      </span>
    </div>
  );
}

export function SecurityNotes() {
  return (
    <>
      <div className="warn-box"><strong>ความปลอดภัย:</strong> ใช้ Virtual Key รายบุคคลเท่านั้น ห้ามใช้ LiteLLM Master Key และห้ามส่ง Screenshot ที่เห็น Key</div>
      <span style={{ fontSize: 12, color: "var(--muted)", lineHeight: 1.6 }}>Key จะถูกเก็บใน Windows Credential Manager ไม่เขียนลงไฟล์หรือ log</span>
    </>
  );
}

export function ModelList({ models, onChange }: { models: ModelConfig[]; onChange: (m: ModelConfig[]) => void }) {
  const [importing, setImporting] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <div className="list-card">
        {models.map((m) => (
          <div className="list-row" key={m.id} style={{ padding: "10px 14px" }}>
            <div className="l">
              <span className="t mono">{m.name}{m.hidden && <span style={{ fontFamily: "var(--font)", fontSize: 11, color: "var(--err-text)", marginLeft: 8 }}>ซ่อน (ทดสอบไม่ผ่าน)</span>}</span>
              <span className="d">context {m.contextWindow.toLocaleString()} · max output {m.maxOutputTokens.toLocaleString()} · effort {m.supportsReasoningEffort.join("/")}</span>
            </div>
            <Switch checked={m.enabled} label={`เปิดใช้ ${m.name}`} onChange={(v) => onChange(models.map((x) => (x.id === m.id ? { ...x, enabled: v } : x)))} />
          </div>
        ))}
      </div>
      <div style={{ display: "flex", flexWrap: "wrap", alignItems: "center", gap: 10 }}>
        <button
          type="button"
          className="btn"
          disabled={importing}
          onClick={async () => {
            const p = await api.pickJson();
            if (!p) return;
            setImporting(true);
            setMsg(null);
            try {
              const imported = await api.importVsCodeModels(p);
              onChange(imported);
              setMsg(`นำเข้า ${imported.length} โมเดลจาก VS Code แล้ว (ไม่ได้อ่าน apiKey)`);
            } catch (e) {
              setMsg((e as { message: string }).message);
            } finally {
              setImporting(false);
            }
          }}
        >
          <Download size={16} strokeWidth={1.8} aria-hidden="true" /> นำเข้าจาก VS Code
        </button>
        <span style={{ fontSize: 12, color: "var(--muted)" }}>เลือกไฟล์ chatLanguageModels.json</span>
      </div>
      {msg && <span style={{ fontSize: 12 }} role="status">{msg}</span>}
      <span style={{ fontSize: 12, color: "var(--muted)", lineHeight: 1.6 }}>
        ค่า context / max output / reasoning effort มาจากคู่มือ Gosoft Agent Gateway — ความสามารถจริงของ backend อาจต่ำกว่า
      </span>
    </div>
  );
}

/** Runs the "Reply exactly: GOSOFT LITELLM OK" test and renders one row per model. */
export function useModelTest() {
  const [results, setResults] = useState<Record<string, TestResult>>({});
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    const off = api.on("gateway-test-result", (r) => setResults((cur) => ({ ...cur, [r.model]: r })));
    return () => { off.then((f) => f()); };
  }, []);
  const run = async () => {
    setResults({});
    setError(null);
    setRunning(true);
    try {
      const all = await api.testModels();
      setResults(Object.fromEntries(all.map((r) => [r.model, r])));
      return all;
    } catch (e) {
      setError((e as { message: string }).message);
      return [];
    } finally {
      setRunning(false);
    }
  };
  return { results, running, error, run };
}

export function TestTable({ models, results, running }: { models: ModelConfig[]; results: Record<string, TestResult>; running: boolean }) {
  const list = models.filter((m) => m.enabled);
  return (
    <div className="list-card" role="table" aria-label="ผลทดสอบโมเดล" aria-busy={running}>
      {list.map((m) => {
        const r = results[m.id];
        const cls = !r ? "" : r.ok ? "" : " fail";
        return (
          <div className={`test-row${cls}`} role="row" key={m.id}>
            <span role="cell" className={`test-icon ${!r ? "wait" : r.ok ? "ok" : "fail"}`} aria-label={!r ? (running ? "กำลังทดสอบ" : "ยังไม่ทดสอบ") : r.ok ? "ผ่าน" : "ไม่ผ่าน"}>
              {!r ? <span className="dot" style={{ background: "var(--muted)" }} /> : r.ok ? <Check size={12} strokeWidth={2.4} /> : <X size={12} strokeWidth={2.4} />}
            </span>
            <span role="cell" className="mono" style={{ width: 90, flex: "none" }}>{m.name}</span>
            <span role="cell" style={{ flex: 1, color: !r ? "var(--muted)" : r.ok ? "var(--muted)" : "var(--err-text)", minWidth: 0, wordBreak: "break-word" }}>
              {!r ? (running ? "กำลังทดสอบ…" : "—") : r.ok ? r.reply : `${r.status ? `${r.status} · ` : ""}${r.error}`}
            </span>
            {r?.ok && <span role="cell" className="muted">{(r.elapsedMs / 1000).toFixed(1)} วิ</span>}
          </div>
        );
      })}
    </div>
  );
}
