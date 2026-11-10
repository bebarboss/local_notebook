import { useEffect, useRef, useState } from "react";
import { Menu, Plus } from "lucide-react";
import type { Effort } from "../lib/types";
import { th } from "../i18n/th";
import { useApp, visibleModels } from "../state";
import { Composer } from "../components/Composer";
import { CodeBadge, SkeletonList } from "../components/ui";

export default function NewChatPage({ onMenu }: { onMenu: () => void }) {
  const app = useApp();
  const { services, stats, settings, go, gateway, activeServiceId, setActiveServiceId, ready } = app;
  const models = visibleModels(app.models);
  // never auto-select; last used service is only *suggested*
  const [picked, setPicked] = useState<string | null>(null);
  const [model, setModel] = useState("");
  const [effort, setEffort] = useState<Effort>("medium");
  const [allServices, setAllServices] = useState(false);
  const mountedActive = useRef(activeServiceId);

  const svc = services.find((s) => s.id === picked) ?? null;

  const pick = (id: string) => {
    const s = services.find((x) => x.id === id);
    setPicked(id);
    setActiveServiceId(id);
    if (s) { setModel(s.model); setEffort(s.effort); }
  };

  // clicking a service in the sidebar while on this page picks it too
  useEffect(() => {
    if (activeServiceId && activeServiceId !== mountedActive.current) {
      mountedActive.current = activeServiceId;
      pick(activeServiceId);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeServiceId]);

  const sendBlocked =
    gateway.state === "unreachable" ? th.err.unreachable :
    gateway.state === "error" ? gateway.message ?? th.gatewayError :
    svc && !models.length ? th.err.noModels : null;

  return (
    <main className="main" aria-label={th.newChat}>
      <div className="topbar only-narrow" style={{ padding: "12px 20px 0" }}>
        <button className="icon-btn" aria-label="เปิดเมนู" onClick={onMenu}><Menu size={18} strokeWidth={1.8} /></button>
      </div>
      <div className="scroll" style={{ display: "flex", flexDirection: "column", justifyContent: "center" }}>
        <div style={{ width: "100%", maxWidth: 720, margin: "0 auto", padding: "40px 20px 8px", display: "flex", flexDirection: "column", gap: 22 }}>
          <div style={{ display: "flex", flexDirection: "column", gap: 6, textAlign: "center" }}>
            <h1 style={{ fontSize: 28, fontWeight: 700 }}>สวัสดี {settings.userName || "คุณ"}</h1>
            <p className="muted" style={{ fontSize: 15 }}>เลือก service ที่จะใช้ค้นเอกสารก่อนเริ่มแชต</p>
          </div>

          {!ready && <SkeletonList rows={2} h={72} />}

          {ready && services.length === 0 && (
            <div className="empty card">
              <h2>ยังไม่มี service</h2>
              <span>สร้าง service แรกโดยเลือกโฟลเดอร์เอกสาร ตั้งชื่อ และเลือกโมเดลเริ่มต้น</span>
              <button className="btn btn-primary btn-lg" onClick={() => go({ name: "services" })}>
                <Plus size={16} strokeWidth={2.2} aria-hidden="true" /> สร้าง service ใหม่
              </button>
            </div>
          )}

          {services.length > 0 && (
            <div className="pick-grid" role="radiogroup" aria-label="เลือก service">
              {services.map((s) => {
                const on = s.id === picked;
                const suggested = !picked && s.id === settings.lastServiceId;
                return (
                  <button
                    key={s.id}
                    role="radio"
                    aria-checked={on}
                    className="pick-card"
                    onClick={() => pick(s.id)}
                    onKeyDown={(e) => {
                      const i = services.findIndex((x) => x.id === s.id);
                      const d = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
                      if (d) {
                        e.preventDefault();
                        const n = services[(i + d + services.length) % services.length];
                        pick(n.id);
                        (e.currentTarget.parentElement?.children[(i + d + services.length) % services.length] as HTMLElement)?.focus();
                      }
                    }}
                  >
                    <CodeBadge code={s.code} color={s.color} size={36} />
                    <span style={{ flex: 1, display: "flex", flexDirection: "column", gap: 2, minWidth: 0 }}>
                      <span style={{ fontWeight: 600 }} className="ellipsis">{s.name}</span>
                      <span style={{ fontSize: 12, color: "var(--muted)" }}>
                        {stats[s.id]?.documents ?? 0} เอกสาร · <span className="mono">{s.model}</span> · {s.effort}
                      </span>
                      {suggested && <span className="suggest">ใช้ล่าสุด</span>}
                    </span>
                    <span className="radio" aria-hidden="true" />
                  </button>
                );
              })}
            </div>
          )}
        </div>
      </div>

      <Composer
        service={svc}
        models={models}
        model={model}
        effort={effort}
        onModelChange={(m, e) => { setModel(m); setEffort(e); }}
        allServices={allServices}
        onToggleAll={() => setAllServices((v) => !v)}
        onSend={async (q) => {
          if (!svc) return;
          await app.updateSettings({ lastServiceId: svc.id });
          go({ name: "chat", chatId: "new", initial: { chatId: null, serviceId: svc.id, question: q, model, effort, allServices } });
        }}
        onStop={() => {}}
        streaming={false}
        sendBlockedReason={sendBlocked}
        placeholder={svc ? `ถามอะไรก็ได้เกี่ยวกับเอกสารใน ${svc.name}` : undefined}
        hint={svc ? `จะค้นเฉพาะเอกสารใน ${svc.name} · เปลี่ยน service ได้ก่อนส่งข้อความแรก` : "แชตแต่ละห้องผูกกับ service เดียว"}
        autoFocus
      />
    </main>
  );
}
