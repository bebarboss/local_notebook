// A docask:// link was clicked (or pasted): a server setup link, or an invite to one shared
// service. Ask once, then the app connects by itself. Only the host / service name is shown
// here — the token or share key stays in Rust and goes to the credential store.
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
  const join = link.kind === "join";

  return (
    <Modal title={join ? "เข้าร่วม service ของทีม" : "เชื่อมต่อเซิร์ฟเวอร์ของทีม"} onClose={busy ? () => {} : close} width={520}>
      {join ? (
        <p style={{ margin: 0, lineHeight: 1.7 }}>
          คุณได้รับเชิญให้ใช้ service
          <br />
          <span style={{ fontWeight: 600, fontSize: 17 }}>{link.serviceName}</span>
          <br />
          <span className="muted" style={{ fontSize: 13 }}>บนเซิร์ฟเวอร์ <span className="mono">{link.host}</span></span>
        </p>
      ) : (
        <p style={{ margin: 0, lineHeight: 1.7 }}>
          ลิงก์นี้จะตั้งให้ DocAsk ใช้เซิร์ฟเวอร์ (ในฐานะเจ้าของ)
          <br />
          <span className="mono" style={{ fontWeight: 600, wordBreak: "break-all" }}>{link.host}</span>
        </p>
      )}
      <ul className="muted" style={{ margin: 0, paddingLeft: 20, fontSize: 13, lineHeight: 1.7 }}>
        {join ? (
          <>
            <li>ถามจากเอกสารของ service นี้ที่ทีมเก็บไว้บนเซิร์ฟเวอร์ได้ทันที</li>
            <li>เพิ่มโฟลเดอร์เอกสารได้ — ข้อความจะถูกส่งขึ้นเซิร์ฟเวอร์ให้ทั้งทีมค้นเจอ</li>
            <li>แชตของคุณยังอยู่ในเครื่องนี้ คนอื่นไม่เห็น</li>
          </>
        ) : (
          <>
            <li>ทำ embedding บนเซิร์ฟเวอร์นี้ และแชร์ service ให้ทีมได้</li>
            <li>รหัสเข้าใช้งานเก็บใน Windows Credential Manager ไม่ต้องกรอกเอง</li>
          </>
        )}
      </ul>
      <p className="muted" style={{ margin: 0, fontSize: 13 }}>กดยืนยันเฉพาะลิงก์ที่ได้จากคนในทีมของคุณเท่านั้น</p>
      {error && <div className="err-box" role="alert">{error}</div>}
      <div className="modal-actions">
        <button type="button" className="btn btn-lg" disabled={busy} onClick={close}>ไม่ใช่ตอนนี้</button>
        <button type="button" className="btn btn-lg btn-primary" disabled={busy} autoFocus onClick={async () => {
          setBusy(true);
          setError(null);
          try {
            const r = await api.connectApply();
            await app.reload();
            setLink(null);
            if (r.service) {
              app.toast(`เข้าร่วม ${r.service.name} แล้ว — ถามได้เลย`);
              app.go({ name: "newchat" });
            } else {
              app.toast("เชื่อมต่อเซิร์ฟเวอร์แล้ว · แชร์ service ได้ที่หน้าแก้ไข service");
            }
          } catch (e) {
            setError((e as { message: string }).message);
          } finally {
            setBusy(false);
          }
        }}>
          <Link2 size={16} strokeWidth={1.8} aria-hidden="true" />{busy ? "กำลังเชื่อมต่อ…" : join ? "เข้าร่วม" : "เชื่อมต่อ"}
        </button>
      </div>
    </Modal>
  );
}
