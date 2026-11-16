import { useEffect, useState } from "react";
import { api } from "../lib/api";
import type { TeamStatus } from "../lib/types";
import { useApp } from "../state";

/** Toasts for background syncs of shared services — app-wide, whatever page is open. */
export function useTeamSyncToasts() {
  const app = useApp();
  useEffect(() => {
    const off = api.on("team-sync", (r) => {
      const name = app.services.find((s) => s.id === r.serviceId)?.name ?? r.serviceId;
      if (r.error) app.toast(`ซิงก์ ${name} ไม่สำเร็จ: ${r.error}`);
      else if (r.uploaded || r.deleted || r.files) app.toast(`ซิงก์ ${name} ขึ้น server แล้ว · ส่ง ${r.uploaded} เอกสาร${r.files ? ` (ต้นฉบับ ${r.files} ไฟล์)` : ""}${r.deleted ? ` · ลบ ${r.deleted}` : ""}${r.tooLarge ? ` · ${r.tooLarge} ไฟล์ใหญ่เกิน 50 MB ไม่ได้ส่งต้นฉบับ` : ""}`);
      if (r.duplicates.length) {
        const d = r.duplicates[0];
        const more = r.duplicates.length > 1 ? ` และอีก ${r.duplicates.length - 1} ไฟล์` : "";
        setTimeout(() => app.toast(`${name}: ไม่ส่ง "${d.file}"${more} — มีในทีมแล้ว (${d.sameAs} จากเครื่อง ${d.from})`), r.uploaded || r.files ? 2800 : 0);
      }
    });
    return () => { off.then((f) => f()); };
  }, [app]);
}

/** For when clicking a docask:// link did not open DocAsk: paste it here instead. */
function PasteLink() {
  const [link, setLink] = useState("");
  const [error, setError] = useState<string | null>(null);
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
        <label className="sr-only" htmlFor="connect-link">ลิงก์ตั้งค่าหรือลิงก์เชิญ</label>
        <input id="connect-link" className="input mono" type="password" autoComplete="off" spellCheck={false} style={{ width: 360, maxWidth: "100%" }}
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
export function TeamPanel() {
  const app = useApp();
  const [st, setSt] = useState<TeamStatus | null>(null);
  const load = () => api.teamStatus().then(setSt).catch(() => {});
  useEffect(() => { load(); }, [app.services.length]);
  useEffect(() => {
    const off = api.on("team-sync", () => load());
    return () => { off.then((f) => f()); };
  }, []);

  return (
    <section aria-labelledby="team-title" style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <h2 id="team-title" style={{ margin: 0 }}>ใช้งานร่วมกันกับทีม</h2>
      <div className="list-card">
        <div className="list-row" style={{ alignItems: "flex-start" }}>
          <div className="l" style={{ gap: 6 }}>
            <span className="d">
              แชร์ทีละ service: เจ้าของกด "ตั้งค่า" ที่การ์ด service แล้วเปิด "แชร์ service นี้" จากนั้นส่ง <b>ลิงก์เชิญ</b> ให้ทีม ·
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
      </div>
    </section>
  );
}
