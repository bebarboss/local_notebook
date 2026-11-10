import { useEffect, useState } from "react";
import { RotateCcw } from "lucide-react";
import { api } from "../lib/api";
import type { GatewayInfo, ModelConfig } from "../lib/types";
import { th } from "../i18n/th";
import { useApp } from "../state";
import { ApiKeyField, keyFormatError, ModelList, SecurityNotes, TestTable, useModelTest } from "../components/GatewayForms";
import { ErrorState, Logo, Skeleton } from "../components/ui";

const TITLES = ["ตั้งค่า Provider", "ใส่ Virtual Key ของคุณ", "เลือกโมเดล", "ทดสอบการเชื่อมต่อ"];

export default function SetupPage() {
  const app = useApp();
  const [step, setStep] = useState(1);
  const [gw, setGw] = useState<GatewayInfo | null>(null);
  const [url, setUrl] = useState("");
  const [key, setKey] = useState("");
  const [models, setModels] = useState<ModelConfig[]>([]);
  const [name, setName] = useState(app.settings.userName);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const test = useModelTest();

  const load = async () => {
    setError(null);
    try {
      const [g, m] = await Promise.all([api.getGateway(), api.listModels()]);
      setGw(g);
      setUrl(g.url);
      setModels(m);
    } catch (e) { setError((e as { message: string }).message); }
  };
  useEffect(() => { load(); }, []);

  useEffect(() => { if (step === 4 && !test.running && Object.keys(test.results).length === 0) test.run(); /* auto-run once */ // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [step]);

  const next = async () => {
    setError(null);
    setBusy(true);
    try {
      if (step === 1) {
        if (!/^https:\/\/[^\s/]+/i.test(url.trim())) throw { message: "Gateway URL ต้องขึ้นต้นด้วย https://" };
        setGw(await api.setGatewayUrl(url.trim()));
        if (name.trim() !== app.settings.userName) await app.updateSettings({ userName: name.trim() });
      } else if (step === 2) {
        if (key) {
          const err = keyFormatError(key);
          if (err) throw { message: err };
          setGw(await api.saveApiKey(key.trim()));
          setKey(""); // drop from memory as soon as Rust has it
        } else if (!gw?.hasKey) throw { message: "กรุณาใส่ API Key" };
      } else if (step === 3) {
        if (!models.some((m) => m.enabled)) throw { message: "เปิดอย่างน้อย 1 โมเดล" };
        setModels(await api.saveModels(models.map((m) => ({ ...m, hidden: false }))));
      }
      setStep((s) => s + 1);
    } catch (e) {
      setError((e as { message: string }).message);
    } finally {
      setBusy(false);
    }
  };

  const finish = async () => {
    await app.updateSettings({ setupDone: true, userName: name.trim() || app.settings.userName });
    await app.refreshModels();
    app.recheckGateway();
    app.go({ name: "newchat" });
  };

  return (
    <main className="setup" aria-label="ตั้งค่าครั้งแรก">
      <Logo size={18} />
      <div className="setup-card" style={{ maxWidth: step === 4 || step === 3 ? 640 : 560 }}>
        <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <span className="step-label">ขั้นที่ {step} จาก 4</span>
          <h1 style={{ fontSize: 24, fontWeight: 700, lineHeight: 1.4 }}>{TITLES[step - 1]}</h1>
          <p className="muted" style={{ lineHeight: 1.7 }}>
            {step === 1 && "ใช้ค่าตามคู่มือ Gosoft Agent Gateway • VS Code User Guide"}
            {step === 2 && "ใช้ Key ที่ได้รับจาก Admin ตามคู่มือ Gosoft Agent Gateway"}
            {step === 3 && "โมเดลทั้ง 6 ตัวตามคู่มือ เปิด/ปิดได้ภายหลังใน ตั้งค่า"}
            {step === 4 && <>ส่ง <code>Reply exactly: GOSOFT LITELLM OK</code> ไปยังแต่ละโมเดล</>}
          </p>
        </div>
        <div className="steps" aria-hidden="true">{[1, 2, 3, 4].map((i) => <div key={i} className={i <= step ? "on" : ""} />)}</div>

        {!gw && !error && <><Skeleton h={40} /><Skeleton h={40} /></>}

        {gw && step === 1 && (
          <>
            <div className="kv">
              <div><span className="k">Group Name</span><span>{gw.groupName}</span></div>
              <div><span className="k">Endpoint Display Name</span><span>{gw.displayName}</span></div>
              <div><span className="k">API Type</span><span>{gw.apiType} <span className="muted" style={{ fontSize: 12 }}>(ล็อก)</span></span></div>
            </div>
            <div className="field">
              <label htmlFor="gw-url">Gateway URL</label>
              <div style={{ display: "flex", gap: 8 }}>
                <input id="gw-url" className="input mono" value={url} onChange={(e) => setUrl(e.target.value)} spellCheck={false} style={{ flex: 1, minWidth: 0 }} />
                <button type="button" className="btn" onClick={() => setUrl(gw.defaultUrl)} disabled={url === gw.defaultUrl}>
                  <RotateCcw size={14} strokeWidth={1.8} aria-hidden="true" />คืนค่าตามคู่มือ
                </button>
              </div>
              <span className="field-hint">base API: <span className="mono">{url.replace(/\/+$/, "")}/v1</span> · เข้าถึงได้เฉพาะตอนต่อ VPN บริษัท</span>
            </div>
            <div className="field">
              <label htmlFor="user-name">ชื่อที่แสดงในแอป</label>
              <input id="user-name" className="input" value={name} onChange={(e) => setName(e.target.value)} placeholder="เช่น ชื่อผู้ใช้ Windows" />
            </div>
          </>
        )}

        {gw && step === 2 && (
          <>
            <div className="kv">
              <div><span className="k">Provider</span><span>{gw.displayName}</span></div>
              <div><span className="k">API Type</span><span>{gw.apiType}</span></div>
            </div>
            <ApiKeyField value={key} onChange={setKey} savedLast4={gw.keyLast4} />
            <SecurityNotes />
          </>
        )}

        {step === 3 && <ModelList models={models} onChange={setModels} />}

        {step === 4 && (
          <>
            <TestTable models={models} results={test.results} running={test.running} />
            {test.error && <ErrorState message={test.error} />}
            <span style={{ fontSize: 12, color: "var(--muted)", lineHeight: 1.6 }}>
              ผลสำเร็จไม่ได้ยืนยันว่า backend ใช้ค่า reasoning effort จริง · โมเดลที่ไม่ผ่านจะถูกซ่อนไว้ก่อน
            </span>
          </>
        )}

        {error && <div className="err-box" role="alert">{error}</div>}

        {step < 4 ? (
          <div style={{ display: "flex", flexWrap: "wrap", justifyContent: "flex-end", gap: 10 }}>
            {step > 1 && <button className="btn btn-lg" onClick={() => { setError(null); setStep((s) => s - 1); }}>{th.back}</button>}
            <button className="btn btn-lg btn-primary" onClick={next} disabled={busy || !gw}>{th.next}</button>
          </div>
        ) : (
          <div style={{ display: "flex", flexWrap: "wrap", justifyContent: "space-between", gap: 10 }}>
            <div style={{ display: "flex", gap: 10 }}>
              <button className="btn btn-lg" onClick={() => setStep(3)} disabled={test.running}>{th.back}</button>
              <button className="btn btn-lg" onClick={() => test.run()} disabled={test.running}>ทดสอบอีกครั้ง</button>
            </div>
            {/* allowed even if some models failed */}
            <button className="btn btn-lg btn-primary" onClick={finish} disabled={test.running}>{th.start}</button>
          </div>
        )}
      </div>
    </main>
  );
}
