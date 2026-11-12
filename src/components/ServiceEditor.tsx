import { useState } from "react";
import { Download, FolderInput, FolderPlus, RefreshCw, Trash2 } from "lucide-react";
import { api, type NewService } from "../lib/api";
import type { Effort, Service } from "../lib/types";
import { th } from "../i18n/th";
import { formatBytes, useApp, visibleModels } from "../state";
import { ConfirmDialog, Modal, Switch } from "./ui";

// All have white-text contrast >= 4.5:1
export const SERVICE_COLORS = ["#1F4FD1", "#B45309", "#0F766E", "#7C3AED", "#BE123C", "#0E7490", "#4D7C0F", "#475569"];

export function ServiceEditor({ service, onClose }: { service: Service | null; onClose: (saved?: Service) => void }) {
  const app = useApp();
  const models = visibleModels(app.models);
  const [form, setForm] = useState<NewService>(() =>
    service
      ? { name: service.name, code: service.code, color: service.color, folders: [...service.folders], model: service.model, effort: service.effort, topK: service.topK, systemPrompt: service.systemPrompt }
      : { name: "", code: "", color: SERVICE_COLORS[app.services.length % SERVICE_COLORS.length], folders: [], model: models[0]?.id ?? "godev1", effort: "medium", topK: app.settings.topK, systemPrompt: "" },
  );
  const [error, setError] = useState<string | null>(null);
  const [exporting, setExporting] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleteChats, setDeleteChats] = useState(false);
  const set = <K extends keyof NewService>(k: K, v: NewService[K]) => setForm((f) => ({ ...f, [k]: v }));

  const codeOk = /^[A-Za-z0-9ก-๙]{2,3}$/.test(form.code.trim());
  const valid = form.name.trim().length > 0 && codeOk && form.topK >= 1 && form.topK <= 30;

  const save = async () => {
    if (!valid) return;
    setBusy(true);
    setError(null);
    try {
      const payload = { ...form, name: form.name.trim(), code: form.code.trim().toUpperCase() };
      const saved = service ? await api.updateService({ ...service, ...payload }) : await api.createService(payload);
      await app.refreshServices();
      onClose(saved);
    } catch (e) {
      setError((e as { message: string }).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title={service ? `ตั้งค่า ${service.name}` : "สร้าง service ใหม่"} onClose={() => onClose()} width={620}>
      <form style={{ display: "flex", flexDirection: "column", gap: 16 }} onSubmit={(e) => { e.preventDefault(); save(); }}>
        <div className="form-grid" style={{ gridTemplateColumns: "1fr 120px" }}>
          <div className="field">
            <label htmlFor="svc-name">ชื่อ</label>
            <input id="svc-name" className="input" value={form.name} onChange={(e) => set("name", e.target.value)} placeholder="เช่น HR · นโยบายบุคคล" required />
          </div>
          <div className="field">
            <label htmlFor="svc-code">รหัสย่อ</label>
            <input id="svc-code" className="input" value={form.code} maxLength={3} onChange={(e) => set("code", e.target.value.toUpperCase())} placeholder="HR" aria-invalid={!!form.code && !codeOk} />
          </div>
        </div>
        {form.code && !codeOk && <span className="field-hint" style={{ color: "var(--err-text)", marginTop: -8 }}>รหัสย่อต้องมี 2–3 ตัวอักษร</span>}

        <div className="field">
          <span className="field-label" id="svc-color-label">สี</span>
          <div className="color-swatches" role="radiogroup" aria-labelledby="svc-color-label">
            {SERVICE_COLORS.map((c) => (
              <button type="button" key={c} role="radio" aria-checked={form.color === c} aria-label={`สี ${c}`} style={{ background: c }} onClick={() => set("color", c)} />
            ))}
          </div>
        </div>

        <div className="field">
          <span className="field-label">โฟลเดอร์ต้นทาง</span>
          <span className="field-hint">แอปจะ index ไฟล์ .pdf .docx .pptx .txt .md .sql และรูปภาพ (OCR) ในโฟลเดอร์ และติดตามไฟล์ใหม่อัตโนมัติ</span>
          {form.folders.map((f) => (
            <div key={f} style={{ display: "flex", gap: 8, alignItems: "center" }}>
              <span className="input mono" style={{ fontSize: 12, display: "flex", alignItems: "center", wordBreak: "break-all" }}>{f}</span>
              {service?.folders.includes(f) && (
                <button type="button" className="icon-btn" aria-label={`ย้ายโฟลเดอร์ ${f}`} title="ย้ายเอกสารไปที่อื่นแล้ว — ชี้ไปที่ใหม่โดยไม่ต้อง index ใหม่"
                  onClick={async () => {
                    const to = await api.pickFolder();
                    if (!to || to === f) return;
                    try {
                      await api.moveServiceFolder(service.id, f, to);
                      set("folders", form.folders.map((x) => (x === f ? to : x)));
                      app.refreshServices();
                      app.toast(`ชี้โฟลเดอร์ไปที่ ${to} แล้ว — ไฟล์ที่เนื้อหาเหมือนเดิมไม่ต้อง index ใหม่`);
                    } catch (e) { setError((e as { message: string }).message); }
                  }}>
                  <FolderInput size={16} strokeWidth={1.8} />
                </button>
              )}
              <button type="button" className="icon-btn" aria-label={`ลบโฟลเดอร์ ${f}`} onClick={() => set("folders", form.folders.filter((x) => x !== f))}>
                <Trash2 size={16} strokeWidth={1.8} />
              </button>
            </div>
          ))}
          <button
            type="button"
            className="btn"
            style={{ alignSelf: "flex-start" }}
            onClick={async () => {
              const f = await api.pickFolder();
              if (f && !form.folders.includes(f)) set("folders", [...form.folders, f]);
            }}
          >
            <FolderPlus size={16} strokeWidth={1.8} aria-hidden="true" /> เพิ่มโฟลเดอร์
          </button>
        </div>

        <div className="form-grid">
          <div className="field">
            <label htmlFor="svc-model">โมเดลเริ่มต้น</label>
            <select id="svc-model" className="select mono" value={form.model} onChange={(e) => set("model", e.target.value)}>
              {!models.some((m) => m.id === form.model) && <option value={form.model}>{form.model}</option>}
              {models.map((m) => <option key={m.id} value={m.id}>{m.name}</option>)}
            </select>
          </div>
          <div className="field">
            <label htmlFor="svc-effort">Reasoning effort</label>
            <select id="svc-effort" className="select" value={form.effort} onChange={(e) => set("effort", e.target.value as Effort)}>
              <option value="low">low</option>
              <option value="medium">medium</option>
              <option value="high">high</option>
            </select>
          </div>
          <div className="field">
            <label htmlFor="svc-topk">top-k (ท่อนที่ส่ง)</label>
            <input id="svc-topk" className="input" type="number" min={1} max={30} value={form.topK} onChange={(e) => set("topK", Number(e.target.value))} />
          </div>
        </div>

        <div className="field">
          <label htmlFor="svc-prompt">System prompt เพิ่มเติม (ไม่บังคับ)</label>
          <textarea id="svc-prompt" className="textarea" rows={3} value={form.systemPrompt} onChange={(e) => set("systemPrompt", e.target.value)} placeholder="เช่น ตอบสั้น กระชับ และอ้างเลขข้อของระเบียบเสมอ" />
        </div>

        {error && <div className="err-box" role="alert">{error}</div>}

        <div className="modal-actions" style={{ justifyContent: service ? "space-between" : "flex-end" }}>
          {service && (
            <div style={{ display: "flex", gap: 10, flexWrap: "wrap" }}>
              <button type="button" className="btn btn-lg btn-danger" onClick={() => setConfirmDelete(true)}>
                <Trash2 size={16} strokeWidth={1.8} aria-hidden="true" /> ลบ service
              </button>
              <button
                type="button"
                className="btn btn-lg"
                disabled={exporting != null}
                title="ไฟล์ .zip เดียว มี index และเอกสารทั้งหมด — ผู้รับนำเข้าแล้วถามได้ทันที"
                onClick={async () => {
                  const dest = await api.pickFolder();
                  if (!dest) return;
                  setExporting(0);
                  const off = api.on("share-progress", (p) => p.phase === "export" && setExporting(p.total ? p.done / p.total : 0));
                  try {
                    const r = await api.exportServiceZip(service.id, dest);
                    app.toast(`ส่งออกแล้ว ${r.documents} ไฟล์ (${formatBytes(r.bytes)}): ${r.path}${r.missing ? ` · ข้าม ${r.missing} ไฟล์ที่ไม่มีแล้ว` : ""}`);
                  } catch (e) { setError((e as { message: string }).message); }
                  finally { setExporting(null); off.then((f) => f()); }
                }}
              >
                <Download size={16} strokeWidth={1.8} aria-hidden="true" />
                {exporting != null ? `กำลังส่งออก ${Math.round(exporting * 100)}%` : "ส่งออก (.zip พร้อมเอกสาร)"}
              </button>
              <button
                type="button"
                className="btn btn-lg"
                onClick={async () => { await api.reindexService(service.id, true); app.toast(`เริ่ม index ${service.code} ใหม่ทั้งหมด`); }}
              >
                <RefreshCw size={16} strokeWidth={1.8} aria-hidden="true" /> Index ใหม่
              </button>
            </div>
          )}
          <div style={{ display: "flex", gap: 10 }}>
            <button type="button" className="btn btn-lg" onClick={() => onClose()}>{th.cancel}</button>
            <button type="submit" className="btn btn-lg btn-primary" disabled={!valid || busy}>{service ? th.save : "สร้าง service"}</button>
          </div>
        </div>
      </form>

      {confirmDelete && service && (
        <ConfirmDialog
          title={`ลบ ${service.name}?`}
          body="index และ vector ของ service นี้จะถูกลบออกจากเครื่อง (ไฟล์ต้นฉบับในโฟลเดอร์ไม่ถูกลบ)"
          extra={
            <label style={{ display: "flex", alignItems: "center", gap: 10, fontSize: 13 }}>
              <Switch checked={deleteChats} onChange={setDeleteChats} label="ลบแชตของ service นี้ด้วย" />
              ลบแชตของ service นี้ด้วย
            </label>
          }
          confirmLabel="ลบ service"
          danger
          onCancel={() => setConfirmDelete(false)}
          onConfirm={async () => {
            await api.deleteService(service.id, deleteChats);
            if (app.activeServiceId === service.id) app.setActiveServiceId(null);
            await app.refreshServices();
            app.bumpChats();
            setConfirmDelete(false);
            onClose();
          }}
        />
      )}
    </Modal>
  );
}
