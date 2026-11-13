// A docask:// setup link was clicked (or pasted): ask once, then the app connects by itself.
// Only the host is shown here — the token stays in Rust and goes to the credential store.
import { useEffect, useState } from "react";
import { Link2 } from "lucide-react";
import { api } from "../lib/api";
import type { ConnectLinkInfo } from "../lib/types";
import { useApp } from "../state";
import { Modal } from "./ui";

export function ConnectDialog() {
  const app = useApp();
  const [link, setLink] = useState<ConnectLinkInfo | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.connectPending().then((l) => l && setLink(l)).catch(() => {});
    const offs = [
      api.on("connect-link", (l) => { setError(null); setLink(l); }),
      api.on("connect-error", (msg) => app.toast(msg)),
    ];
    return () => { offs.forEach((o) => o.then((f) => f())); };
  }, [app]);

  if (!link) return null;
  const close = () => { api.connectDiscard().catch(() => {}); setLink(null); };

  return (
    <Modal title="เชื่อมต่อเซิร์ฟเวอร์ของทีม" onClose={busy ? () => {} : close} width={520}>
      <p style={{ margin: 0, lineHeight: 1.7 }}>
        ลิงก์นี้จะตั้งให้ DocAsk ใช้เซิร์ฟเวอร์
        <br />
        <span className="mono" style={{ fontWeight: 600, wordBreak: "break-all" }}>{link.host}</span>
      </p>
      <ul className="muted" style={{ margin: 0, paddingLeft: 20, fontSize: 13, lineHeight: 1.7 }}>
        <li>ทำ embedding และค้นเอกสารจากฐานข้อมูลของทีมบนเซิร์ฟเวอร์นี้</li>
        <li>ข้อความในเอกสารที่ทำ index บนเครื่องนี้จะถูกส่งขึ้นเซิร์ฟเวอร์นี้</li>
        <li>รหัสเข้าใช้งานเก็บใน Windows Credential Manager ไม่ต้องกรอกเอง</li>
      </ul>
      <p className="muted" style={{ margin: 0, fontSize: 13 }}>กดเชื่อมต่อเฉพาะลิงก์ที่ได้จากผู้ดูแลเซิร์ฟเวอร์ของทีมเท่านั้น</p>
      {error && <div className="err-box" role="alert">{error}</div>}
      <div className="modal-actions">
        <button type="button" className="btn btn-lg" disabled={busy} onClick={close}>ไม่เชื่อมต่อ</button>
        <button type="button" className="btn btn-lg btn-primary" disabled={busy} autoFocus onClick={async () => {
          setBusy(true);
          setError(null);
          try {
            const r = await api.connectApply();
            await app.reload();
            setLink(null);
            app.toast(r.teamError ? `เชื่อมต่อเซิร์ฟเวอร์แล้ว แต่ยังเปิดฐานข้อมูลของทีมไม่ได้: ${r.teamError}` : "เชื่อมต่อเซิร์ฟเวอร์ของทีมแล้ว · กำลังซิงก์ index");
          } catch (e) {
            setError((e as { message: string }).message);
          } finally {
            setBusy(false);
          }
        }}>
          <Link2 size={16} strokeWidth={1.8} aria-hidden="true" />{busy ? "กำลังเชื่อมต่อ…" : "เชื่อมต่อ"}
        </button>
      </div>
    </Modal>
  );
}
