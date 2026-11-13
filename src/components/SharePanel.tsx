// "แชร์ service นี้": the owner shares a service on the team database and hands out invite
// links; a member who joined with an invite can leave. Lives in the service editor.
import { useEffect, useState } from "react";
import { Copy, KeyRound, LogOut, Users } from "lucide-react";
import { api } from "../lib/api";
import type { Service } from "../lib/types";
import { useApp } from "../state";
import { ConfirmDialog, Switch } from "./ui";

export function SharePanel({ service: initial, onLeft }: { service: Service; onLeft: () => void }) {
  const app = useApp();
  // the live copy: sharing changes it while the editor stays open
  const service = app.services.find((s) => s.id === initial.id) ?? initial;
  const [owner, setOwner] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ask, setAsk] = useState<"share" | "unshare" | "rotate" | "leave" | null>(null);
  const shared = !!service.shared;

  useEffect(() => {
    api.teamStatus().then((s) => setOwner(s.owner)).catch(() => setOwner(false));
  }, []);

  const copy = async (link: string, msg: string) => {
    // the invite carries the service's key — copied only when the user asks, for the team
    await navigator.clipboard.writeText(link);
    app.toast(msg);
  };
  const run = async (f: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try { await f(); } catch (e) { setError((e as { message: string }).message); } finally { setBusy(false); }
  };

  if (owner === null) return null;

  return (
    <div className="field">
      <span className="field-label" style={{ display: "flex", alignItems: "center", gap: 6 }}>
        <Users size={14} strokeWidth={1.8} aria-hidden="true" /> ใช้งานร่วมกับทีม
      </span>

      {!shared && owner && (
        <div style={{ display: "flex", alignItems: "center", gap: 10, justifyContent: "space-between" }}>
          <span className="field-hint" style={{ margin: 0 }}>
            เก็บ index ของ service นี้ไว้บนเซิร์ฟเวอร์ แล้วส่งลิงก์เชิญให้ทีม — คนในทีมถามและเพิ่มเอกสารได้ เห็นเฉพาะ service นี้
          </span>
          <Switch checked={false} disabled={busy} label="แชร์ service นี้ให้ทีม" onChange={() => setAsk("share")} />
        </div>
      )}
      {!shared && !owner && (
        <span className="field-hint">
          แชร์ได้เฉพาะเครื่องที่เชื่อมต่อเซิร์ฟเวอร์ด้วยลิงก์ตั้งค่าของผู้ดูแล · ถ้าได้รับลิงก์เชิญ กดลิงก์นั้นเพื่อเข้าร่วม service ของทีม
        </span>
      )}

      {shared && owner && (
        <>
          <span className="field-hint">
            <span className="ok-text">แชร์อยู่</span> · ค้นจากเซิร์ฟเวอร์ · เอกสารในโฟลเดอร์ของเครื่องนี้ถูกส่งขึ้นหลังทำ index ทุกครั้ง ·
            ใครมีลิงก์เชิญก็เข้า service นี้ได้ ส่งเฉพาะคนในทีม
          </span>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button type="button" className="btn btn-primary" disabled={busy} onClick={() => run(async () => copy(await api.serviceInvite(service.id), "คัดลอกลิงก์เชิญแล้ว — ส่งให้คนในทีม"))}>
              <Copy size={16} strokeWidth={1.8} aria-hidden="true" /> คัดลอกลิงก์เชิญ
            </button>
            <button type="button" className="btn" disabled={busy} onClick={() => setAsk("rotate")}>
              <KeyRound size={16} strokeWidth={1.8} aria-hidden="true" /> สร้างคีย์ใหม่
            </button>
            <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => setAsk("unshare")}>หยุดแชร์</button>
          </div>
        </>
      )}
      {shared && !owner && (
        <>
          <span className="field-hint">
            <span className="ok-text">เข้าร่วมจากทีม</span> · ค้นจากเซิร์ฟเวอร์ · เพิ่มโฟลเดอร์ด้านบนได้ เอกสารจะถูกส่งขึ้นให้ทั้งทีม · แชตของคุณอยู่ในเครื่องนี้
          </span>
          <button type="button" className="btn btn-ghost" style={{ alignSelf: "flex-start" }} disabled={busy} onClick={() => setAsk("leave")}>
            <LogOut size={16} strokeWidth={1.8} aria-hidden="true" /> ออกจาก service
          </button>
        </>
      )}
      {error && <span className="field-hint" style={{ color: "var(--warn-text)" }} role="alert">{error}</span>}

      {ask === "share" && (
        <ConfirmDialog title={`แชร์ ${service.name} ให้ทีม?`}
          body="index ของ service นี้ (ข้อความในเอกสาร) จะถูกส่งขึ้นฐานข้อมูลบนเซิร์ฟเวอร์ และได้ลิงก์เชิญสำหรับส่งให้ทีม · คำถามของ service นี้จะค้นจากเซิร์ฟเวอร์"
          confirmLabel="แชร์"
          onCancel={() => setAsk(null)}
          onConfirm={() => { setAsk(null); run(async () => {
            const link = await api.shareService(service.id, true);
            await app.refreshServices();
            if (link) await copy(link, "แชร์แล้ว · คัดลอกลิงก์เชิญแล้ว — กำลังส่ง index ขึ้นเซิร์ฟเวอร์");
          }); }} />
      )}
      {ask === "rotate" && (
        <ConfirmDialog title="สร้างคีย์ใหม่?"
          body="ลิงก์เชิญเดิมทุกลิงก์จะใช้ไม่ได้ทันที — คนที่เข้าร่วมแล้วจะค้น service นี้ไม่ได้จนกว่าจะกดลิงก์ใหม่ · ใช้เมื่อลิงก์หลุดหรือมีคนออกจากทีม"
          confirmLabel="สร้างคีย์ใหม่"
          onCancel={() => setAsk(null)}
          onConfirm={() => { setAsk(null); run(async () => copy(await api.rotateServiceKey(service.id), "สร้างคีย์ใหม่แล้ว · คัดลอกลิงก์เชิญใหม่แล้ว — ส่งให้คนที่ยังอยู่ในทีม")); }} />
      )}
      {ask === "unshare" && (
        <ConfirmDialog title={`หยุดแชร์ ${service.name}?`}
          body="เครื่องนี้จะกลับไปค้นจาก index ในเครื่อง · ข้อมูลบนเซิร์ฟเวอร์และลิงก์เชิญยังอยู่ คนในทีมยังใช้ได้ (ถ้าจะตัดทุกคนออก ให้กด สร้างคีย์ใหม่ ก่อน)"
          confirmLabel="หยุดแชร์"
          onCancel={() => setAsk(null)}
          onConfirm={() => { setAsk(null); run(async () => { await api.shareService(service.id, false); await app.refreshServices(); app.toast("หยุดแชร์แล้ว"); }); }} />
      )}
      {ask === "leave" && (
        <ConfirmDialog title={`ออกจาก ${service.name}?`}
          body="service นี้จะหายจากเครื่องนี้และลืมคีย์ไป · ทีมยังใช้ต่อได้ตามปกติ · เข้าร่วมใหม่ได้ด้วยลิงก์เชิญ"
          confirmLabel="ออกจาก service"
          onCancel={() => setAsk(null)}
          onConfirm={() => { setAsk(null); run(async () => { await api.leaveService(service.id); await app.refreshServices(); app.toast(`ออกจาก ${service.name} แล้ว`); onLeft(); }); }} />
      )}
    </div>
  );
}
