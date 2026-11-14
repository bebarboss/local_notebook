import { useEffect, useState } from "react";
import { ArrowLeft, FolderOpen, Download } from "lucide-react";
import { api } from "../lib/api";
import type { UpdateInfo, AppInfo, DownloadProgress, EmbeddingModelId, EmbeddingStatus, EmbedServerInfo, EmbedServerTest, TeamStatus, GatewayInfo, ModelConfig, RerankStatus, ThemeMode, FontChoice, TextSize } from "../lib/types";
import { th } from "../i18n/th";
import { formatBytes, useApp, type SettingsSection } from "../state";
import { ApiKeyField, keyFormatError, ModelList, SecurityNotes, TestTable, useModelTest } from "../components/GatewayForms";
import { TroubleshootTable } from "../components/TroubleshootTable";
import { ProgressBar } from "../components/ProgressBar";
import { StatusPill } from "../components/StatusPill";
import { ConfirmDialog, ErrorState, Modal, Skeleton, Switch } from "../components/ui";
import { UpdateDialog } from "../components/UpdateDialog";

const SECTIONS: { id: SettingsSection; label: string }[] = [
  { id: "gateway", label: "Gateway และ Key" },
  { id: "models", label: "โมเดล" },
  { id: "embedding", label: "Embedding และ Index" },
  { id: "search", label: "การค้นหา" },
  { id: "history", label: "ประวัติแชต" },
  { id: "theme", label: "ธีม" },
  { id: "startup", label: "เปิดโปรแกรม" },
  { id: "trouble", label: "วิธีแก้ปัญหา" },
  { id: "about", label: "เกี่ยวกับ" },
];

export default function SettingsPage({ section = "gateway" }: { section?: SettingsSection }) {
  const { go } = useApp();
  const [cur, setCur] = useState<SettingsSection>(section);
  useEffect(() => setCur(section), [section]);
  return (
    <main className="page" aria-label={th.settings}>
      <div className="page-inner" style={{ maxWidth: 980 }}>
        <div className="l" style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          <button className="back-link" onClick={() => go({ name: "newchat" })}><ArrowLeft size={14} strokeWidth={1.8} aria-hidden="true" />กลับไปแชต</button>
          <h1 className="page-title">ตั้งค่า</h1>
        </div>
        <div className="settings-layout">
          <nav className="settings-nav" aria-label="หมวดการตั้งค่า">
            {SECTIONS.map((s) => (
              <button key={s.id} aria-current={cur === s.id ? "page" : undefined} onClick={() => setCur(s.id)}>{s.label}</button>
            ))}
          </nav>
          <div className="settings-main">
            {cur === "gateway" && <GatewaySection />}
            {cur === "models" && <ModelsSection />}
            {cur === "embedding" && <EmbeddingSection />}
            {cur === "search" && <SearchSection />}
            {cur === "history" && <HistorySection />}
            {cur === "theme" && <section><h2>ธีม</h2><div className="list-card"><ThemeRow /><FontRow /><TextSizeRow /></div></section>}
            {cur === "startup" && <section><h2>เปิดโปรแกรม</h2><div className="list-card"><StartupRows /></div></section>}
            {cur === "trouble" && <section><h2>ตรวจสอบและแก้ปัญหา</h2><TroubleshootTable /></section>}
            {cur === "about" && <AboutSection />}
          </div>
        </div>
      </div>
    </main>
  );
}

function ThemeRow() {
  const { settings, updateSettings } = useApp();
  const opts: { v: ThemeMode; label: string }[] = [{ v: "light", label: "สว่าง" }, { v: "dark", label: "มืด" }, { v: "system", label: "ตามระบบ" }];
  return (
    <div className="list-row">
      <div className="l"><span className="t">ธีม</span><span className="d">ตามระบบ Windows หรือเลือกเอง</span></div>
      <div className="segmented" role="radiogroup" aria-label="ธีม">
        {opts.map((o) => <button key={o.v} role="radio" aria-checked={settings.theme === o.v} onClick={() => updateSettings({ theme: o.v })}>{o.label}</button>)}
      </div>
    </div>
  );
}

function StartupRows() {
  const { settings, updateSettings, toast } = useApp();
  const [on, setOn] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.getAutostart().then(setOn).catch(() => setOn(false));
  }, []);
  return (
    <>
      <div className="list-row">
        <div className="l">
          <span className="t">เปิดพร้อม Windows</span>
          <span className="d">เปิด DocAsk เองเมื่อเข้าสู่ระบบ Windows · ทำ index ไฟล์ที่เปลี่ยนระหว่างปิดเครื่องให้เสร็จก่อนจะใช้ · ตั้งเฉพาะผู้ใช้นี้ ไม่ต้องสิทธิ์ admin</span>
          {error && <span className="d" style={{ color: "var(--err-text)" }}>{error}</span>}
        </div>
        <Switch
          checked={!!on}
          disabled={on == null}
          label="เปิดพร้อม Windows"
          onChange={async (v) => {
            setError(null);
            try {
              const r = await api.setAutostart(v);
              setOn(r);
              toast(r ? "DocAsk จะเปิดเองเมื่อเข้าสู่ระบบ Windows" : "ปิดการเปิดพร้อม Windows แล้ว");
            } catch (e) {
              setError((e as { message: string }).message);
            }
          }}
        />
      </div>
      <div className="list-row">
        <div className="l">
          <span className="t">ซ่อนไว้ที่ถาดตอนเปิดพร้อม Windows</span>
          <span className="d">ไม่เด้งหน้าต่างขึ้นมา รออยู่ที่ไอคอนข้างนาฬิกา (คลิกเพื่อเปิด หรือเปิดจากไอคอนบนเดสก์ท็อป) · ปิดตัวเลือกนี้ถ้าอยากให้หน้าต่างเปิดขึ้นเลย</span>
        </div>
        <Switch checked={settings.startHidden} disabled={!on} label="ซ่อนไว้ที่ถาดตอนเปิดพร้อม Windows" onChange={(v) => updateSettings({ startHidden: v })} />
      </div>
    </>
  );
}

function FontRow() {
  const { settings, updateSettings } = useApp();
  const opts: { v: FontChoice; label: string; family: string }[] = [
    { v: "looped", label: "มีหัว (แนะนำ)", family: '"IBM Plex Sans Thai Looped"' },
    { v: "sarabun", label: "สารบรรณ", family: '"Sarabun"' },
    { v: "plex", label: "ไม่มีหัว", family: '"IBM Plex Sans Thai"' },
    { v: "system", label: "ของ Windows", family: '"Leelawadee UI"' },
  ];
  return (
    <div className="list-row">
      <div className="l"><span className="t">ฟอนต์</span><span className="d">แบบมีหัวอ่านข้อความยาวง่ายที่สุด · สารบรรณคล้ายหนังสือราชการ</span></div>
      <div className="segmented" role="radiogroup" aria-label="ฟอนต์">
        {opts.map((o) => <button key={o.v} role="radio" aria-checked={settings.font === o.v} style={{ fontFamily: o.family }} onClick={() => updateSettings({ font: o.v })}>{o.label}</button>)}
      </div>
    </div>
  );
}

function TextSizeRow() {
  const { settings, updateSettings } = useApp();
  const opts: { v: TextSize; label: string }[] = [{ v: "s", label: "เล็ก" }, { v: "m", label: "ปกติ" }, { v: "l", label: "ใหญ่" }, { v: "xl", label: "ใหญ่มาก" }];
  return (
    <div className="list-row">
      <div className="l"><span className="t">ขนาดตัวอักษร</span><span className="d">ขนาดของคำตอบในแชต (ส่วนอื่นขยายตามเล็กน้อย)</span></div>
      <div className="segmented" role="radiogroup" aria-label="ขนาดตัวอักษร">
        {opts.map((o) => <button key={o.v} role="radio" aria-checked={settings.textSize === o.v} onClick={() => updateSettings({ textSize: o.v })}>{o.label}</button>)}
      </div>
    </div>
  );
}

function GatewaySection() {
  const app = useApp();
  const [gw, setGw] = useState<GatewayInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [updateKey, setUpdateKey] = useState(false);
  const [editUrl, setEditUrl] = useState<string | null>(null);
  const [removing, setRemoving] = useState(false);
  const [testing, setTesting] = useState(false);
  const load = () => api.getGateway().then(setGw).catch((e) => setError(e.message));
  useEffect(() => { load(); }, []);
  const visible = app.models.filter((m) => m.enabled && !m.hidden).length;
  const total = app.models.filter((m) => m.enabled).length;

  if (error) return <ErrorState message={error} onRetry={load} />;
  if (!gw) return <section><Skeleton h={260} style={{ borderRadius: 14 }} /></section>;

  return (
    <section>
      <h2>{gw.displayName}</h2>
      <div className="list-card">
        <div className="list-row">
          <div className="l">
            <span className="t">Gateway URL</span>
            {editUrl === null ? (
              <span className="d mono" style={{ wordBreak: "break-all" }}>{gw.url}</span>
            ) : (
              <form style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 6 }} onSubmit={async (e) => {
                e.preventDefault();
                try { setGw(await api.setGatewayUrl(editUrl)); setEditUrl(null); await app.refreshModels(); app.recheckGateway(); } catch (er) { app.toast((er as { message: string }).message); }
              }}>
                <label className="sr-only" htmlFor="set-url">Gateway URL</label>
                <input id="set-url" className="input mono" value={editUrl} onChange={(e) => setEditUrl(e.target.value)} style={{ flex: "1 1 260px" }} />
                <button type="button" className="btn" onClick={() => setEditUrl(gw.defaultUrl)}>คืนค่าตามคู่มือ</button>
                <button type="submit" className="btn btn-primary">{th.save}</button>
              </form>
            )}
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
            <StatusPill status={app.gateway} okText={`เชื่อมต่อได้ · ${visible}/${total} โมเดล`} />
            {editUrl === null && <button className="btn" onClick={() => setEditUrl(gw.url)}>แก้ไข</button>}
          </div>
        </div>
        <div className="list-row">
          <div className="l">
            <span className="t">API Key</span>
            <span className="d mono">{gw.hasKey ? `sk-•••••••••••• ${gw.keyLast4 ?? ""} · Windows Credential Manager` : "ยังไม่ได้ใส่ Key"}</span>
          </div>
          <button className="btn btn-primary" onClick={() => setUpdateKey(true)}>Update API Key</button>
        </div>
        <div className="list-row">
          <div className="l">
            <span className="t">ทดสอบการเชื่อมต่อ</span>
            <span className="d">
              {gw.lastTestAt ? `ทดสอบล่าสุด ${new Date(gw.lastTestAt).toLocaleString("th-TH", { dateStyle: "short", timeStyle: "short" })} · ผ่าน ${gw.lastTestPassed} จาก ${gw.lastTestTotal}` : "ยังไม่เคยทดสอบ"}
            </span>
          </div>
          <button className="btn" onClick={() => setTesting(true)}>ทดสอบ</button>
        </div>
        <ThemeRow />
        <div className="list-row">
          <div className="l"><span className="t">ลบ Provider</span><span className="d">ลบ Key และการตั้งค่าโมเดลออกจากเครื่อง</span></div>
          <button className="btn btn-danger" onClick={() => setRemoving(true)}>Remove Provider</button>
        </div>
      </div>

      {updateKey && <UpdateKeyDialog info={gw} onClose={() => { setUpdateKey(false); load(); app.refreshModels(); }} />}
      {testing && <TestDialog onClose={() => { setTesting(false); load(); app.refreshModels(); app.recheckGateway(); }} />}
      {removing && (
        <ConfirmDialog
          title="ลบ Provider?"
          body="Key จะถูกลบจาก Windows Credential Manager และการตั้งค่า Gateway/โมเดลจะถูกรีเซ็ต แชตและเอกสารยังอยู่ จากนั้นแอปจะเปิด Setup Wizard"
          confirmLabel="Remove Provider"
          danger
          onCancel={() => setRemoving(false)}
          onConfirm={async () => {
            await api.removeProvider();
            setRemoving(false);
            await app.reload();
          }}
        />
      )}
    </section>
  );
}

/** Enter a new key and re-test without redoing the 6 models. */
function UpdateKeyDialog({ info, onClose }: { info: GatewayInfo; onClose: () => void }) {
  const [key, setKey] = useState("");
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const test = useModelTest();
  const { models } = useApp();
  return (
    <Modal title="Update API Key" onClose={onClose} width={640}>
      {!saved ? (
        <form style={{ display: "flex", flexDirection: "column", gap: 16 }} onSubmit={async (e) => {
          e.preventDefault();
          const err = keyFormatError(key);
          if (err) { setError(err); return; }
          try {
            await api.saveApiKey(key.trim());
            setKey("");
            setSaved(true);
            test.run();
          } catch (er) { setError((er as { message: string }).message); }
        }}>
          <ApiKeyField value={key} onChange={(v) => { setKey(v); setError(null); }} savedLast4={null} />
          <span className="field-hint">Key ปัจจุบัน: <span className="mono">sk-•••• {info.keyLast4 ?? "—"}</span> · บันทึกแล้วจะทดสอบโมเดลเดิมทันที</span>
          <SecurityNotes />
          {error && <div className="err-box" role="alert">{error}</div>}
          <div className="modal-actions">
            <button type="button" className="btn btn-lg" onClick={onClose}>{th.cancel}</button>
            <button type="submit" className="btn btn-lg btn-primary" disabled={!key}>บันทึกและทดสอบ</button>
          </div>
        </form>
      ) : (
        <>
          <span className="ok-text" style={{ fontSize: 13 }}>✓ บันทึก Key ใหม่ใน Windows Credential Manager แล้ว</span>
          <TestTable models={models} results={test.results} running={test.running} />
          {test.error && <ErrorState message={test.error} />}
          <div className="modal-actions">
            <button className="btn btn-lg" onClick={() => test.run()} disabled={test.running}>ทดสอบอีกครั้ง</button>
            <button className="btn btn-lg btn-primary" onClick={onClose} disabled={test.running}>เสร็จ</button>
          </div>
        </>
      )}
    </Modal>
  );
}

function TestDialog({ onClose }: { onClose: () => void }) {
  const test = useModelTest();
  const { models } = useApp();
  useEffect(() => { test.run(); /* eslint-disable-next-line react-hooks/exhaustive-deps */ }, []);
  return (
    <Modal title="ทดสอบการเชื่อมต่อ" onClose={onClose} width={640}>
      <p className="muted">ส่ง <code>Reply exactly: GOSOFT LITELLM OK</code> ไปยังแต่ละโมเดล</p>
      <TestTable models={models} results={test.results} running={test.running} />
      {test.error && <ErrorState message={test.error} />}
      <span style={{ fontSize: 12, color: "var(--muted)" }}>ผลสำเร็จไม่ได้ยืนยันว่า backend ใช้ค่า reasoning effort จริง · โมเดลที่ไม่ผ่านจะถูกซ่อนไว้ก่อน</span>
      <div className="modal-actions">
        <button className="btn btn-lg" onClick={() => test.run()} disabled={test.running}>ทดสอบอีกครั้ง</button>
        <button className="btn btn-lg btn-primary" onClick={onClose} disabled={test.running}>{th.close}</button>
      </div>
    </Modal>
  );
}

function ModelsSection() {
  const app = useApp();
  const [models, setModels] = useState<ModelConfig[]>(app.models);
  const [dirty, setDirty] = useState(false);
  useEffect(() => { setModels(app.models); }, [app.models]);
  return (
    <section>
      <h2>โมเดล</h2>
      <ModelList models={models} onChange={(m) => { setModels(m); setDirty(true); }} />
      <div style={{ display: "flex", gap: 10, flexWrap: "wrap" }}>
        <button className="btn btn-primary" disabled={!dirty} onClick={async () => { await api.saveModels(models); await app.refreshModels(); setDirty(false); app.toast("บันทึกโมเดลแล้ว"); }}>{th.save}</button>
        <button className="btn" onClick={async () => { setModels(await api.resetModels()); await app.refreshModels(); setDirty(false); }}>คืนค่า 6 โมเดลตามคู่มือ</button>
        <button className="btn" onClick={async () => {
          const unhide = models.map((m) => ({ ...m, hidden: false }));
          await api.saveModels(unhide); await app.refreshModels(); setDirty(false); app.toast("แสดงโมเดลที่ซ่อนแล้ว — ควรทดสอบอีกครั้ง");
        }}>แสดงโมเดลที่ซ่อน</button>
      </div>
      <span className="field-hint">ไฟล์ config: %APPDATA%\DocAsk\models.json (ไม่มี Key ในไฟล์นี้)</span>
    </section>
  );
}

/** the only local embedding model (also the server's model, so either can stand in for the other) */
const LOCAL_MODEL: EmbeddingModelId = "bge-m3";
const LOCAL_SIZE = "~570 MB";

function EmbeddingSection() {
  const app = useApp();
  const [st, setSt] = useState<EmbeddingStatus | null>(null);
  const model = LOCAL_MODEL;
  // on the server, downloading / picking a local model would switch away from it
  const onServer = app.settings.embeddingModel.startsWith("server:");
  const [dl, setDl] = useState<DownloadProgress | null>(null);
  const [confirmReindex, setConfirmReindex] = useState(false);
  const load = () => api.embeddingStatus().then(setSt);
  useEffect(() => { load(); }, []);
  useEffect(() => {
    const off = api.on("embed-download", (p) => {
      if (p.model === RERANK_MODEL) return; // shown by RerankRow
      setDl(p.done || p.error ? null : p);
      if (p.error) app.toast(p.error);
      if (p.done) { load(); app.toast("ดาวน์โหลดโมเดลเสร็จแล้ว"); }
    });
    return () => { off.then((f) => f()); };
  }, [app]);
  return (
    <section>
      <h2>Embedding และ Index</h2>
      <div className="list-card">
        <div className="list-row">
          <div className="l">
            <span className="t">โมเดลที่ใช้อยู่</span>
            <span className="d">{st ? <>{st.model} · {st.ready ? <span className="ok-text">พร้อมใช้งาน</span> : <span style={{ color: "var(--warn-text)" }}>{st.model.startsWith("gateway:") ? "ยังไม่มี API key" : st.model.startsWith("server:") ? "ยังไม่ได้ตั้ง URL / token ของเซิร์ฟเวอร์" : "ยังไม่มีไฟล์โมเดล — กดดาวน์โหลด"}</span>}{st.sizeBytes ? ` · ${formatBytes(st.sizeBytes)}` : ""}</> : "…"}</span>
            {st?.path && <span className="d mono" style={{ wordBreak: "break-all" }}>{st.path}</span>}
          </div>
        </div>
        {!onServer && (<>
        <div className="list-row">
          <div className="l">
            <span className="t">(ก) ดาวน์โหลด bge-m3 จาก Hugging Face</span>
            <span className="d">โมเดลในเครื่อง bge-m3 (int8) {LOCAL_SIZE} · เก็บที่ %APPDATA%\DocAsk\models\ · ดาวน์โหลดครั้งเดียว</span>
            {dl && <div style={{ marginTop: 6 }}><ProgressBar value={dl.total ? dl.downloaded / dl.total : 0} label="ดาวน์โหลดโมเดล" /><span className="d">{dl.file} · {formatBytes(dl.downloaded)}{dl.total ? ` / ${formatBytes(dl.total)}` : ""}</span></div>}
          </div>
          <button className="btn" disabled={!!dl} onClick={async () => {
            try { await app.updateSettings({ embeddingModel: model }); await api.downloadEmbedding(model); } catch (e) { app.toast((e as { message: string }).message); }
          }}><Download size={16} strokeWidth={1.8} aria-hidden="true" />ดาวน์โหลด</button>
        </div>
        <div className="list-row">
          <div className="l">
            <span className="t">(ข) เลือกโฟลเดอร์โมเดลที่ดาวน์โหลดไว้แล้ว</span>
            <span className="d">สำหรับเครือข่ายที่บล็อก huggingface.co — โฟลเดอร์ต้องมี onnx/model_quantized.onnx (หรือ model.onnx) และ tokenizer.json</span>
          </div>
          <button className="btn" onClick={async () => {
            const f = await api.pickFolder();
            if (!f) return;
            try { await app.updateSettings({ embeddingModel: model }); setSt(await api.useEmbeddingFolder(model, f)); app.toast("ใช้โมเดลจากโฟลเดอร์แล้ว"); } catch (e) { app.toast((e as { message: string }).message); }
          }}><FolderOpen size={16} strokeWidth={1.8} aria-hidden="true" />เลือกโฟลเดอร์</button>
        </div>
        </>)}
        <EmbedServerRow onChanged={setSt} />
        <TeamRow />
        <IndexLoadRow />
        <OcrRow />
        <KeepImagesRow />
        <div className="list-row">
          <div className="l"><span className="t">Index ใหม่ทั้งหมด</span><span className="d">อ่านทุกไฟล์ใหม่ทุก service · ข้อความที่ไม่เปลี่ยนใช้ vector เดิม (เปลี่ยนโมเดลแล้วจึงทำ embedding ใหม่ทั้งหมด)</span></div>
          <button className="btn" disabled={!st?.ready} onClick={() => setConfirmReindex(true)}>Index ใหม่ทั้งหมด</button>
        </div>
      </div>
      {confirmReindex && (
        <ConfirmDialog title="Index ใหม่ทั้งหมด?" body="จะใช้เวลาตามจำนวนเอกสาร ระหว่างนี้ยังใช้แชตได้แต่ผลค้นอาจไม่ครบ" confirmLabel="เริ่ม index"
          onCancel={() => setConfirmReindex(false)}
          onConfirm={async () => { await api.reindexAll(); setConfirmReindex(false); app.toast("เริ่ม index ใหม่ทุก service"); }} />
      )}
    </section>
  );
}

function NumberRow({ label, desc, value, min, max, step = 1, onSave }: { label: string; desc: string; value: number; min: number; max: number; step?: number; onSave: (v: number) => void }) {
  const [v, setV] = useState(String(value));
  useEffect(() => setV(String(value)), [value]);
  const id = `num-${label}`;
  const n = Number(v);
  const ok = Number.isFinite(n) && n >= min && n <= max;
  return (
    <div className="list-row">
      <div className="l"><label className="t" htmlFor={id}>{label}</label><span className="d">{desc}</span></div>
      <input id={id} className="input" type="number" min={min} max={max} step={step} value={v} aria-invalid={!ok}
        style={{ width: 120 }} onChange={(e) => setV(e.target.value)} onBlur={() => ok && n !== value && onSave(n)} />
    </div>
  );
}

function SearchSection() {
  const { settings, updateSettings } = useApp();
  return (
    <section>
      <h2>การค้นหา</h2>
      <div className="list-card">
        <NumberRow label="top-k เริ่มต้น" desc="ค่าเริ่มต้นของ service ใหม่ (แต่ละ service ตั้งเองได้)" value={settings.topK} min={1} max={30} onSave={(v) => updateSettings({ topK: v })} />
        <NumberRow label="Threshold" desc="ตัดท่อนที่คะแนนความเกี่ยวข้องต่ำกว่าค่านี้ (0–1)" value={settings.threshold} min={0} max={1} step={0.05} onSave={(v) => updateSettings({ threshold: v })} />
        <HybridRow />
        <RerankRow />
        <NumberRow label="ขนาดท่อน (ตัวอักษร)" desc="เปลี่ยนแล้วจะ index ใหม่ทั้งหมดในรอบถัดไป" value={settings.chunkSize} min={300} max={4000} step={50} onSave={(v) => updateSettings({ chunkSize: v })} />
        <NumberRow label="Overlap (ตัวอักษร)" desc="ส่วนที่ซ้อนกันระหว่างท่อน · เปลี่ยนแล้วจะ index ใหม่ทั้งหมด" value={settings.chunkOverlap} min={0} max={1000} step={10} onSave={(v) => updateSettings({ chunkOverlap: v })} />
      </div>
    </section>
  );
}

function HistorySection() {
  const app = useApp();
  const { settings, updateSettings } = app;
  const [confirm, setConfirm] = useState(false);
  return (
    <section>
      <h2>ประวัติแชต</h2>
      <div className="list-card">
        <NumberRow label="จำนวนรอบที่ส่งไปด้วย" desc="ส่งคำถาม/คำตอบ N รอบล่าสุดไปกับคำถามใหม่ (คำตอบเก่าถูกย่อให้สั้นเมื่อยาวเกิน) · รอบที่เก่ากว่านั้นถูกสรุปเป็นไม่กี่บรรทัดแล้วส่งไปแทน · 0 = ไม่ส่งประวัติและไม่สรุป" value={settings.historyTurns} min={0} max={20} onSave={(v) => updateSettings({ historyTurns: v })} />
        <div className="list-row">
          <div className="l"><span className="t">ลบแชตอัตโนมัติ</span><span className="d">ลบแชตที่ไม่ได้ใช้เกินระยะเวลาที่กำหนด</span></div>
          <div className="segmented" role="radiogroup" aria-label="ลบแชตอัตโนมัติ">
            {[{ v: 0, l: "ไม่ลบ" }, { v: 30, l: "30 วัน" }, { v: 90, l: "90 วัน" }].map((o) => (
              <button key={o.v} role="radio" aria-checked={settings.autoDeleteDays === o.v} onClick={() => updateSettings({ autoDeleteDays: o.v })}>{o.l}</button>
            ))}
          </div>
        </div>
        <div className="list-row">
          <div className="l"><span className="t">ลบแชตทั้งหมด</span><span className="d">ลบประวัติแชตทุก service ออกจากเครื่อง</span></div>
          <button className="btn btn-danger" onClick={() => setConfirm(true)}>ลบทั้งหมด</button>
        </div>
      </div>
      {confirm && (
        <ConfirmDialog title="ลบแชตทั้งหมด?" body="ไม่สามารถกู้คืนได้" confirmLabel="ลบทั้งหมด" danger onCancel={() => setConfirm(false)}
          onConfirm={async () => { await api.deleteAllChats(); app.bumpChats(); setConfirm(false); app.go({ name: "settings", section: "history" }); app.toast("ลบแชตทั้งหมดแล้ว"); }} />
      )}
    </section>
  );
}

function AboutSection() {
  const app = useApp();
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [wipe, setWipe] = useState(false);
  useEffect(() => { api.appInfo().then(setInfo); }, []);
  return (
    <section>
      <h2>เกี่ยวกับ</h2>
      <div className="list-card">
        <div className="list-row"><div className="l"><span className="t">DocAsk</span><span className="d">เวอร์ชัน {info?.version ?? "…"}{info?.mock ? " · MOCK MODE" : ""}</span></div></div>
        <div className="list-row"><div className="l"><span className="t">ที่เก็บข้อมูล</span><span className="d mono" style={{ wordBreak: "break-all" }}>{info?.dataDir ?? "…"}</span></div></div>
        <UpdateRows />
        <div className="list-row">
          <div className="l"><span className="t">Setup Wizard</span><span className="d">เปิดขั้นตอนตั้งค่าครั้งแรกอีกครั้ง</span></div>
          <button className="btn" onClick={() => app.go({ name: "setup" })}>เปิด</button>
        </div>
        <div className="list-row">
          <div className="l"><span className="t">ลบข้อมูลทั้งหมด</span><span className="d">ลบ service, index, แชต, config และ Key ออกจากเครื่องนี้ (ไฟล์ต้นฉบับไม่ถูกลบ)</span></div>
          <button className="btn btn-danger" onClick={() => setWipe(true)}>ลบข้อมูลทั้งหมด</button>
        </div>
      </div>
      {wipe && (
        <ConfirmDialog title="ลบข้อมูลทั้งหมด?" body="ข้อมูลทั้งหมดใน %APPDATA%\DocAsk และ Key ใน Credential Manager จะถูกลบ ไม่สามารถกู้คืนได้" confirmLabel="ลบทั้งหมด" danger
          onCancel={() => setWipe(false)} onConfirm={async () => { await api.wipeAllData(); setWipe(false); await app.reload(); }} />
      )}
    </section>
  );
}

function UpdateRows() {
  const { settings, updateSettings, toast } = useApp();
  const [url, setUrl] = useState(settings.updateUrl);
  const [checking, setChecking] = useState(false);
  const [result, setResult] = useState<string | null>(null);
  const [found, setFound] = useState<UpdateInfo | null>(null);
  useEffect(() => setUrl(settings.updateUrl), [settings.updateUrl]);
  const urlOk = url.trim() === "" || /^https:\/\/\S+$/.test(url.trim());

  const check = async () => {
    setChecking(true);
    setResult(null);
    try {
      const u = await api.checkUpdate();
      if (u.available) setFound(u);
      else setResult(`ใช้เวอร์ชันล่าสุดอยู่แล้ว (${u.currentVersion})`);
    } catch (e) {
      setResult((e as { message: string }).message);
    } finally {
      setChecking(false);
    }
  };

  return (
    <>
      <div className="list-row">
        <div className="l">
          <span className="t">ตรวจหาอัปเดต</span>
          <span className="d">{result ?? "ดาวน์โหลดและติดตั้งเวอร์ชันใหม่ในแอป ไฟล์ต้องมีลายเซ็นของ DocAsk เท่านั้น"}</span>
        </div>
        <button className="btn" onClick={check} disabled={checking || !settings.updateUrl}>{checking ? "กำลังตรวจ…" : "ตรวจหาอัปเดต"}</button>
      </div>
      <div className="list-row">
        <div className="l">
          <label className="t" htmlFor="update-url">URL อัปเดต</label>
          <span className="d">ไฟล์ latest.json บน server ของบริษัท (https) · เว้นว่าง = ปิดการอัปเดตในแอป</span>
          <form style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 6 }} onSubmit={async (e) => {
            e.preventDefault();
            if (!urlOk) return;
            await updateSettings({ updateUrl: url.trim() });
            toast("บันทึก URL อัปเดตแล้ว");
          }}>
            <input id="update-url" className="input mono" value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://.../docask/latest.json" aria-invalid={!urlOk} style={{ flex: "1 1 280px", fontSize: 12 }} />
            <button type="submit" className="btn" disabled={!urlOk || url.trim() === settings.updateUrl}>{th.save}</button>
          </form>
          {!urlOk && <span className="d" style={{ color: "var(--err-text)" }}>ต้องขึ้นต้นด้วย https://</span>}
        </div>
      </div>
      <div className="list-row">
        <div className="l"><span className="t">ตรวจอัปเดตอัตโนมัติ</span><span className="d">ตรวจตอนเปิดแอป (ต้องต่อ VPN ถ้า server อยู่ในเครือข่ายบริษัท)</span></div>
        <Switch checked={settings.autoCheckUpdate} label="ตรวจอัปเดตอัตโนมัติ" onChange={(v) => updateSettings({ autoCheckUpdate: v })} />
      </div>
      {found && <UpdateDialog info={found} onClose={() => setFound(null)} />}
    </>
  );
}

const RERANK_MODEL = "mmarco-mMiniLMv2-L12" as const;

function RerankRow() {
  const { settings, updateSettings, toast } = useApp();
  const [st, setSt] = useState<RerankStatus | null>(null);
  const [dl, setDl] = useState<DownloadProgress | null>(null);
  const load = () => api.rerankStatus().then(setSt).catch(() => {});
  useEffect(() => { load(); }, []);
  useEffect(() => {
    const off = api.on("embed-download", (p) => {
      if (p.model !== RERANK_MODEL) return;
      setDl(p.done || p.error ? null : p);
      if (p.error) toast(p.error);
      if (p.done) { load(); toast("ดาวน์โหลดโมเดล reranker เสร็จแล้ว"); }
    });
    return () => { off.then((f) => f()); };
  }, [toast]);
  return (
    <div className="list-row" style={{ alignItems: "flex-start" }}>
      <div className="l">
        <span className="t">จัดลำดับใหม่ (reranking)</span>
        <span className="d">
          ค้นด้วย vector ได้ท่อนที่ใกล้เคียง {Math.max(15, Math.min(30, settings.topK * 3))} ท่อน แล้วให้โมเดล reranker อ่านคำถามคู่กับแต่ละท่อนเพื่อเลือก top-k ที่ตรงที่สุด ·
          แม่นขึ้นชัดเจนกับคำถามภาษาไทย แต่ใช้เวลาเพิ่มราว 1–2 วินาทีต่อคำถาม · ทำงานในเครื่อง
        </span>
        <span className="d">
          {st ? (st.ready
            ? <><span className="ok-text">พร้อมใช้งาน</span> · {st.model}{st.sizeBytes ? ` · ${formatBytes(st.sizeBytes)}` : ""}</>
            : <span style={{ color: "var(--warn-text)" }}>ยังไม่มีโมเดล reranker (~120 MB)</span>) : "…"}
        </span>
        {dl && <div style={{ marginTop: 6 }}><ProgressBar value={dl.total ? dl.downloaded / dl.total : 0} label="ดาวน์โหลดโมเดล reranker" /><span className="d">{dl.file} · {formatBytes(dl.downloaded)}{dl.total ? ` / ${formatBytes(dl.total)}` : ""}</span></div>}
        {st && !st.ready && (
          <div style={{ display: "flex", gap: 8, marginTop: 8, flexWrap: "wrap" }}>
            <button className="btn" disabled={!!dl} onClick={() => api.downloadReranker().catch((e) => toast((e as { message: string }).message))}>
              <Download size={16} strokeWidth={1.8} aria-hidden="true" />ดาวน์โหลด
            </button>
            <button className="btn" onClick={async () => {
              const f = await api.pickFolder();
              if (!f) return;
              try { setSt(await api.useRerankerFolder(f)); toast("ใช้โมเดล reranker จากโฟลเดอร์แล้ว"); } catch (e) { toast((e as { message: string }).message); }
            }}><FolderOpen size={16} strokeWidth={1.8} aria-hidden="true" />เลือกโฟลเดอร์</button>
          </div>
        )}
      </div>
      <Switch
        checked={settings.rerank}
        label="จัดลำดับใหม่ (reranking)"
        onChange={async (v) => {
          await updateSettings({ rerank: v });
          toast(v ? (st?.ready ? "เปิด reranking แล้ว" : "เปิดแล้ว — ดาวน์โหลดโมเดลก่อนจึงจะทำงาน") : "ปิด reranking แล้ว");
        }}
      />
    </div>
  );
}

function HybridRow() {
  const { settings, updateSettings, toast } = useApp();
  return (
    <div className="list-row">
      <div className="l">
        <span className="t">ค้นด้วยคำตรงตัวด้วย (hybrid search)</span>
        <span className="d">
          ค้นตามความหมาย (vector) คู่กับค้นคำที่ตรงตัว แล้วรวมผล · ช่วยมากกับรหัส เลขที่สัญญา ชื่อคน ชื่อเฉพาะ ·
          ทำงานในเครื่อง แทบไม่ช้าลง
        </span>
      </div>
      <Switch
        checked={settings.hybrid}
        label="ค้นด้วยคำตรงตัวด้วย (hybrid search)"
        onChange={async (v) => {
          await updateSettings({ hybrid: v });
          toast(v ? "เปิด hybrid search แล้ว" : "ปิดแล้ว — ค้นตามความหมายอย่างเดียว");
        }}
      />
    </div>
  );
}

/** questions fall back to the same model on this PC while the server is down */
function FallbackLine({ r, onDownloaded }: { r: EmbedServerTest; onDownloaded: () => void }) {
  const app = useApp();
  const [dl, setDl] = useState<DownloadProgress | null>(null);
  useEffect(() => {
    if (!r.fallbackModel) return;
    const off = api.on("embed-download", (p) => {
      if (p.model !== r.fallbackModel) return;
      setDl(p.done || p.error ? null : p);
      if (p.done) onDownloaded(); // re-test: now with the local comparison
    });
    return () => { off.then((f) => f()); };
  }, [r.fallbackModel, onDownloaded]);

  if (!r.fallbackModel) {
    return <span className="d">สำรองในเครื่อง: ไม่มี — แอปไม่มีโมเดล {r.model} แบบในเครื่อง ถ้าเซิร์ฟเวอร์ล่มจะค้นเอกสารไม่ได้</span>;
  }
  if (!r.fallbackReady) {
    const size = r.fallbackModel === LOCAL_MODEL ? LOCAL_SIZE : "";
    return (
      <span className="d" style={{ display: "flex", flexDirection: "column", gap: 6 }}>
        <span style={{ color: "var(--warn-text)" }}>สำรองในเครื่อง: ยังไม่มี {r.fallbackModel} บนเครื่องนี้ — ถ้าเซิร์ฟเวอร์ล่มจะถามไม่ได้</span>
        {dl ? (
          <><ProgressBar value={dl.total ? dl.downloaded / dl.total : 0} label={`ดาวน์โหลด ${r.fallbackModel}`} /><span>{dl.file} · {formatBytes(dl.downloaded)}{dl.total ? ` / ${formatBytes(dl.total)}` : ""}</span></>
        ) : (
          <button className="btn" style={{ alignSelf: "flex-start" }} onClick={() => {
            // download only — the embedding source stays on the server
            api.downloadEmbedding(r.fallbackModel!).catch((e) => app.toast((e as { message: string }).message));
          }}><Download size={16} strokeWidth={1.8} aria-hidden="true" />ดาวน์โหลด {r.fallbackModel} ไว้สำรอง {size && <span className="muted">{size}</span>}</button>
        )}
      </span>
    );
  }
  const m = r.fallbackMatch;
  return (
    <span className="d">
      สำรองในเครื่อง: <span className="ok-text">{r.fallbackModel} พร้อม</span> · ถ้าเซิร์ฟเวอร์ล่ม คำถามจะคำนวณในเครื่องแทน (ทำ index ใหม่ต้องรอเซิร์ฟเวอร์)
      {m !== null && <> · ผลตรงกับเซิร์ฟเวอร์ {m.toFixed(3)}{m < 0.95 && <span style={{ color: "var(--warn-text)" }}> · ต่างกันมาก ผลค้นตอนสำรองอาจไม่ดี</span>}</>}
    </span>
  );
}

/** For when clicking a docask:// link did not open DocAsk: paste it here instead. */
function PasteLink() {
  const [link, setLink] = useState("");
  const [error, setError] = useState<string | null>(null);
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
        <label className="sr-only" htmlFor="connect-link">ลิงก์ตั้งค่าจากผู้ดูแลเซิร์ฟเวอร์</label>
        <input id="connect-link" className="input mono" type="password" autoComplete="off" spellCheck={false} style={{ width: 360 }}
          placeholder="วางลิงก์ตั้งค่า / ลิงก์เชิญ docask://… (ถ้ากดลิงก์แล้วไม่เปิดแอป)" value={link} onChange={(e) => setLink(e.target.value)} />
        <button className="btn" disabled={!link.trim()} onClick={async () => {
          setError(null);
          try { await api.connectPaste(link.trim()); setLink(""); } catch (e) { setError((e as { message: string }).message); }
        }}>ใช้ลิงก์</button>
      </div>
      {error && <span className="d" style={{ color: "var(--warn-text)" }} role="alert">{error}</span>}
    </div>
  );
}

/** The team database: who this PC is (owner / member), what is shared, links, sync. */
function TeamRow() {
  const app = useApp();
  const [st, setSt] = useState<TeamStatus | null>(null);
  const load = () => api.teamStatus().then(setSt).catch(() => {});
  useEffect(() => { load(); }, [app.services.length]);
  useEffect(() => {
    const off = api.on("team-sync", (r) => {
      const name = app.services.find((s) => s.id === r.serviceId)?.name ?? r.serviceId;
      if (r.error) app.toast(`ซิงก์ ${name} ไม่สำเร็จ: ${r.error}`);
      else if (r.uploaded || r.deleted) app.toast(`ซิงก์ ${name} ขึ้น server แล้ว · ส่ง ${r.uploaded} ไฟล์${r.deleted ? ` · ลบ ${r.deleted}` : ""}`);
      load();
    });
    return () => { off.then((f) => f()); };
  }, [app]);

  return (
    <div className="list-row" style={{ alignItems: "flex-start" }}>
      <div className="l" style={{ gap: 6 }}>
        <span className="t">ใช้งานร่วมกันกับทีม</span>
        <span className="d">
          แชร์ทีละ service: เจ้าของเปิด "แชร์ service นี้" ในหน้าแก้ไข service แล้วส่ง <b>ลิงก์เชิญ</b> ให้ทีม ·
          คนที่กดลิงก์เชิญเห็นเฉพาะ service นั้น ถามและเพิ่มเอกสารได้ · แชตเป็นของแต่ละคน
        </span>
        {st && (
          <span className="d">
            เครื่องนี้: <b>{st.owner ? "เจ้าของเซิร์ฟเวอร์" : "สมาชิก (เข้าร่วมด้วยลิงก์เชิญ)"}</b> · ชื่อ <span className="mono">{st.machine}</span> ·
            service ที่แชร์อยู่ {st.sharedServices}
            {st.info && <> · บน server {st.info.documents.toLocaleString()} เอกสาร / {st.info.chunks.toLocaleString()} ท่อน</>}
          </span>
        )}
        {st?.error && <span className="d" style={{ color: "var(--warn-text)" }}>ยังต่อฐานข้อมูลไม่ได้: {st.error}</span>}
        {!!st?.sharedServices && (
          <button className="btn" style={{ alignSelf: "flex-start" }} onClick={async () => {
            try { await api.teamSyncNow(); app.toast("กำลังซิงก์ service ที่แชร์ขึ้น server…"); } catch (e) { app.toast((e as { message: string }).message); }
          }}>ซิงก์ตอนนี้</button>
        )}
        <PasteLink />
      </div>
    </div>
  );
}

function EmbedServerRow({ onChanged }: { onChanged: (s: EmbeddingStatus) => void }) {
  const app = useApp();
  const current = app.settings.embeddingModel.startsWith("server:") ? app.settings.embeddingModel.slice(7) : "";
  const [info, setInfo] = useState<EmbedServerInfo | null>(null);
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  const [test, setTest] = useState<{ result?: EmbedServerTest; error?: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [leave, setLeave] = useState(false);
  useEffect(() => {
    api.embedServerInfo().then((i) => { setInfo(i); setUrl(i.url); }).catch(() => {});
  }, []);

  const saveAndTest = async () => {
    setBusy(true);
    setTest(null);
    try {
      const i = await api.setEmbedServer(url, token.trim() || null);
      setInfo(i);
      setUrl(i.url);
      setToken(""); // the token now lives in Windows Credential Manager only
      setTest({ result: await api.testEmbedServer() });
    } catch (e) {
      setTest({ error: (e as { message: string }).message });
    } finally {
      setBusy(false);
    }
  };

  const r = test?.result;
  const dirty = !!info && (url.trim() !== info.url || !!token.trim());
  return (
    <div className="list-row" style={{ alignItems: "flex-start" }}>
      <div className="l" style={{ gap: 6 }}>
        <span className="t">หรือใช้เซิร์ฟเวอร์ embedding ของตัวเอง</span>
        <span className="d">
          เช่น Docker บน Mac mini ที่บ้าน (โฟลเดอร์ server/embed ใน repo) · ข้อความทุกท่อนของเอกสารถูกส่งไปเครื่องนั้นตอนทำ index (และคำถามตอนค้นหา) ·
          ต่อผ่าน ngrok หรือ Tailscale · token เก็บใน Windows Credential Manager · ถ้าเครื่องนั้นปิด จะใช้โมเดลในเครื่องสำรอง (ถ้ามี)
        </span>
        {info?.url && (
          <span className="d">
            <span className="mono" style={{ wordBreak: "break-all" }}>{info.url}</span>
            {info.hasToken ? ` · token ····${info.tokenLast4 ?? ""}` : " · ยังไม่มี token"}
          </span>
        )}
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
          <button className="btn" disabled={busy || !url.trim() || (!info?.hasToken && !token.trim())} onClick={saveAndTest}>
            {busy ? "กำลังทดสอบ…" : dirty ? "บันทึกและทดสอบ" : "ทดสอบ"}
          </button>
          {r && (
            <button className="btn btn-primary" disabled={busy || r.model === current} onClick={() => setConfirm(true)}>ใช้เซิร์ฟเวอร์นี้</button>
          )}
        </div>
        {/* normally a setup link fills these in; by hand only when the tunnel URL changed etc. */}
        <details open={!info?.url || undefined}>
          <summary className="d" style={{ cursor: "pointer" }}>ตั้งค่าเอง (ขั้นสูง) — ปกติใช้ลิงก์ตั้งค่าจากผู้ดูแลเซิร์ฟเวอร์แทน</summary>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center", marginTop: 8 }}>
            <label className="sr-only" htmlFor="es-url">URL ของเซิร์ฟเวอร์ embedding</label>
            <input id="es-url" className="input mono" placeholder="https://xxxx.trycloudflare.com" value={url} onChange={(e) => setUrl(e.target.value)} style={{ width: 300 }} spellCheck={false} autoComplete="off" />
            <label className="sr-only" htmlFor="es-token">Token</label>
            <input id="es-token" className="input mono" type="password" autoComplete="off" spellCheck={false} style={{ width: 230 }}
              placeholder={info?.hasToken ? `มีแล้ว ····${info.tokenLast4 ?? ""}` : "token จาก .env ของเซิร์ฟเวอร์"}
              title={info?.hasToken ? "ใส่ token ใหม่เพื่อเปลี่ยน · เว้นว่างไว้ = ใช้ token เดิม" : undefined}
              value={token} onChange={(e) => setToken(e.target.value)} />
          </div>
        </details>
        {test?.error && <span className="d" style={{ color: "var(--warn-text)" }} role="alert">ใช้ไม่ได้: {test.error}</span>}
        {r && <FallbackLine r={r} onDownloaded={saveAndTest} />}
        {r && (
          <span className="d">
            <span className="ok-text">ใช้ได้</span> · <span className="mono">{r.model}</span> · {r.dims} มิติ · {r.device} · {r.ms} ms ·
            ประโยคความหมายเดียวกัน {r.similar.toFixed(2)} / คนละเรื่อง {r.unrelated.toFixed(2)}
            {r.similar - r.unrelated < 0.1 && <span style={{ color: "var(--warn-text)" }}> · แยกความหมายภาษาไทยได้ไม่ดี</span>}
          </span>
        )}
        {current && (
          <span className="d" style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
            กำลังใช้ <span className="mono">{current}</span> บนเซิร์ฟเวอร์
            <button className="btn btn-ghost" disabled={busy} onClick={() => setLeave(true)}>เลิกใช้เซิร์ฟเวอร์</button>
          </span>
        )}
        {!current && info?.url && (
          <button className="btn btn-ghost" style={{ alignSelf: "flex-start" }} onClick={async () => {
            try { const i = await api.forgetEmbedServer(); setInfo(i); setUrl(i.url); setTest(null); app.toast(i.builtin ? "กลับไปใช้ค่าที่มากับโปรแกรมแล้ว" : "ลบ URL และ token ของเซิร์ฟเวอร์แล้ว"); } catch (e) { app.toast((e as { message: string }).message); }
          }}>{info.builtin ? "กลับไปใช้ URL ที่มากับโปรแกรม" : "ลบ URL และ token"}</button>
        )}
      </div>
      {leave && (
        <ConfirmDialog title="เลิกใช้เซิร์ฟเวอร์?"
          body="กลับไปทำ embedding ด้วย bge-m3 ในเครื่องนี้ และปิดฐานข้อมูลของทีม · ต้องดาวน์โหลด bge-m3 (~570 MB) ถ้ายังไม่มี และทำ embedding ใหม่ทุก service ในเครื่อง"
          confirmLabel="เลิกใช้เซิร์ฟเวอร์"
          onCancel={() => setLeave(false)}
          onConfirm={async () => {
            setLeave(false);
            setBusy(true);
            try {
              await app.updateSettings({ embeddingModel: LOCAL_MODEL, teamServer: false });
              onChanged(await api.embeddingStatus());
              app.toast("กลับไปใช้โมเดลในเครื่องแล้ว — ถ้ายังไม่มีไฟล์โมเดล กดดาวน์โหลดด้านบน");
            } catch (e) {
              app.toast((e as { message: string }).message);
            } finally {
              setBusy(false);
            }
          }} />
      )}
      {confirm && r && (
        <ConfirmDialog title={`ใช้ ${r.model} บนเซิร์ฟเวอร์?`}
          body="ข้อความทุกท่อนของทุก service จะถูกส่งไปเซิร์ฟเวอร์เพื่อทำ embedding ใหม่ทั้งหมด (vector เดิมใช้ต่อไม่ได้) · ถ้าเครื่องที่บ้านปิดหรือ Tailscale หลุด จะค้นเอกสารไม่ได้"
          confirmLabel="เปลี่ยนและ index ใหม่"
          onCancel={() => setConfirm(false)}
          onConfirm={async () => {
            setConfirm(false);
            setBusy(true);
            try {
              const st = await api.useEmbedServer(r.model);
              await app.updateSettings({ embeddingModel: `server:${r.model}` });
              onChanged(st);
              app.toast(`เปลี่ยนเป็น ${r.model} บนเซิร์ฟเวอร์แล้ว — เริ่ม index ใหม่ทุก service`);
            } catch (e) {
              app.toast((e as { message: string }).message);
            } finally {
              setBusy(false);
            }
          }} />
      )}
    </div>
  );
}

const LOADS = [
  { v: "light", l: "เบา", toast: "ทำ index แบบเบา — ช้าลงแต่เครื่องไม่หนัก" },
  { v: "normal", l: "ปกติ", toast: "ทำ index แบบปกติ" },
  { v: "fast", l: "เร็ว", toast: "ทำ index เต็มกำลัง" },
] as const;

function IndexLoadRow() {
  const { settings, updateSettings, toast } = useApp();
  return (
    <div className="list-row">
      <div className="l">
        <span className="t">ความแรงตอนทำ index</span>
        <span className="d">
          เบา: ใช้ CPU ราว 15% และพักเป็นช่วงๆ ช้ากว่าปกติประมาณ 2 เท่า · ปกติ: ราวครึ่งเครื่อง (แนะนำ) ·
          เร็ว: เต็มที่ เร็วขึ้นเฉพาะเครื่องที่มีหลาย core · ทุกระดับให้โปรแกรมอื่นได้ CPU ก่อน · OCR เปลี่ยนตามหลังเปิดแอปใหม่
        </span>
      </div>
      <div className="segmented" role="radiogroup" aria-label="ความแรงตอนทำ index">
        {LOADS.map((o) => (
          <button key={o.v} role="radio" aria-checked={settings.indexLoad === o.v}
            onClick={async () => { await updateSettings({ indexLoad: o.v }); toast(o.toast); }}>{o.l}</button>
        ))}
      </div>
    </div>
  );
}

function KeepImagesRow() {
  const { settings, updateSettings, toast } = useApp();
  return (
    <div className="list-row">
      <div className="l">
        <span className="t">แสดงรูปจากเอกสาร</span>
        <span className="d">
          เก็บรูปใน PDF / Word / PowerPoint และหน้าสแกนไว้ในเครื่อง แล้วแสดงเมื่อเปิดแหล่งอ้างอิง ·
          index ใหญ่ขึ้นราว 50–150 KB ต่อรูป · รูปไม่ถูกส่งไป Gateway และไม่ขึ้นฐานข้อมูลของทีม (เห็นเฉพาะเครื่องที่ทำ index) · เปิด/ปิดแล้วจะ index ใหม่ทั้งหมด
        </span>
      </div>
      <Switch
        checked={settings.keepImages}
        label="แสดงรูปจากเอกสาร"
        onChange={async (v) => {
          await updateSettings({ keepImages: v });
          toast(v ? "เปิดแล้ว — เอกสารจะถูก index ใหม่เพื่อเก็บรูป" : "ปิดแล้ว — รูปที่เก็บไว้จะถูกลบตอน index ใหม่");
        }}
      />
    </div>
  );
}

function OcrRow() {
  const { settings, updateSettings, toast } = useApp();
  const [available, setAvailable] = useState<boolean | null>(null);
  useEffect(() => { api.ocrStatus().then((s) => setAvailable(s.available)).catch(() => setAvailable(false)); }, []);
  return (
    <div className="list-row">
      <div className="l">
        <span className="t">OCR ข้อความในรูปภาพ</span>
        <span className="d">
          อ่านข้อความจาก PDF สแกน, รูปภาพ (.png .jpg ฯลฯ) และรูปใน PDF / Word / PowerPoint เช่น screenshot หน้าจอระบบ ·
          ไทย + อังกฤษ (PaddleOCR) ทำงานในเครื่อง · เปิด/ปิดแล้วจะ index ใหม่ทั้งหมด
        </span>
        {available === false && <span className="d" style={{ color: "var(--warn-text)" }}>ไม่พบไฟล์โมเดล OCR ในตัวติดตั้ง</span>}
      </div>
      <Switch
        checked={settings.ocrEnabled}
        label="OCR ข้อความในรูปภาพ"
        onChange={async (v) => {
          await updateSettings({ ocrEnabled: v });
          toast(v ? "เปิด OCR แล้ว — เอกสารจะถูก index ใหม่" : "ปิด OCR แล้ว");
        }}
      />
    </div>
  );
}
