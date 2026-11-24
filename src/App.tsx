import { useEffect, useState } from "react";
import { X } from "lucide-react";
import { api } from "./lib/api";
import { th } from "./i18n/th";
import { useApp } from "./state";
import { Sidebar } from "./components/Sidebar";
import { ErrorState, Logo, SkeletonList } from "./components/ui";
import ChatPage from "./pages/Chat";
import NewChatPage from "./pages/NewChat";
import ServicesPage from "./pages/Services";
import SettingsPage from "./pages/Settings";
import SetupPage from "./pages/Setup";
import { UpdateDialog } from "./components/UpdateDialog";
import { ConnectDialog } from "./components/ConnectDialog";
import { useTeamSyncToasts } from "./components/TeamPanel";
import { KnowledgeCompareDialog } from "./components/KnowledgeCompareDialog";
import type { KnowledgeDuplicate, SimilarNote, UpdateInfo } from "./lib/types";

export default function App() {
  const app = useApp();
  const { page, gateway, ready, loadError } = app;
  const [menuOpen, setMenuOpen] = useState(false);
  const [needEmbedding, setNeedEmbedding] = useState(false);
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [updateOpen, setUpdateOpen] = useState(false);
  const [updateDismissed, setUpdateDismissed] = useState(false);
  useTeamSyncToasts();
  // "มีบันทึกเรื่องนี้อยู่แล้ว": stays until opened or dismissed
  const [dups, setDups] = useState<KnowledgeDuplicate[]>([]);
  const [comparing, setComparing] = useState<{ dup: KnowledgeDuplicate; other: SimilarNote } | null>(null);
  useEffect(() => {
    const off = api.on("knowledge-duplicate", (d) => setDups((xs) => [...xs.filter((x) => x.path !== d.path), d]));
    return () => { off.then((f) => f()); };
  }, []);
  const dropDup = (path: string) => setDups((xs) => xs.filter((x) => x.path !== path));

  // check for a new version once at startup (silently ignore VPN/URL problems)
  useEffect(() => {
    if (!ready || !app.settings.setupDone || !app.settings.autoCheckUpdate || !app.settings.updateUrl) return;
    const t = setTimeout(() => {
      api.checkUpdate().then((u) => u.available && setUpdate(u)).catch(() => {});
    }, 4000);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready, app.settings.setupDone]);

  // first run: the embedding model is not bundled — ask the user to download or pick a folder
  useEffect(() => {
    if (!ready || page.name === "setup") return;
    const check = () => api.embeddingStatus().then((s) => setNeedEmbedding(!s.ready)).catch(() => {});
    check();
    const off = api.on("embed-download", (p) => { if (p.done) check(); });
    return () => { off.then((f) => f()); };
  }, [ready, page.name, app.settings.embeddingModel]);

  if (loadError) {
    return (
      <div className="setup">
        <Logo />
        <div style={{ maxWidth: 520, width: "100%" }}><ErrorState message={loadError} onRetry={app.reload} /></div>
        {api.mock && <span className="mock-badge">{th.mock}</span>}
      </div>
    );
  }

  if (!ready) {
    return (
      <div className="app" aria-busy="true">
        <aside className="sidebar"><Logo /><SkeletonList rows={6} h={32} /></aside>
        <main className="main" style={{ padding: 40 }}><SkeletonList rows={3} h={72} /></main>
      </div>
    );
  }

  if (page.name === "setup") {
    return (
      <div className="app">
        <SetupPage />
        <ConnectDialog />
        {api.mock && <span className="mock-badge">{th.mock}</span>}
      </div>
    );
  }

  const showBanner = gateway.state === "unreachable" || gateway.state === "error";

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
      {showBanner && (
        <div className={`banner${gateway.state === "error" ? " warn" : ""}`} role="alert">
          <span>
            {gateway.state === "unreachable" ? th.err.unreachable : gateway.message ?? th.gatewayError}
            {" · "}ยังค้นและดูเอกสาร/แชตเก่าได้ แต่ส่งคำถามใหม่ไม่ได้
          </span>
          <button className="btn" onClick={app.recheckGateway}>{th.tryAgain}</button>
        </div>
      )}
      {needEmbedding && !(page.name === "settings" && page.section === "embedding") && (
        <div className="banner warn" role="status">
          <span>ยังไม่มีโมเดล embedding — ใช้ลิงก์ตั้งค่าจากผู้ดูแลเซิร์ฟเวอร์ของทีม หรือดาวน์โหลด bge-m3 ในเครื่อง (~570 MB) ก่อน จึงจะ index และค้นเอกสารได้</span>
          <button className="btn" onClick={() => app.go({ name: "settings", section: "embedding" })}>ตั้งค่า Embedding</button>
        </div>
      )}
      {update && !updateDismissed && (
        <div className="banner info" role="status">
          <span>มี DocAsk เวอร์ชันใหม่ <b className="mono">{update.version}</b> (ตอนนี้ใช้ {update.currentVersion})</span>
          <button className="btn btn-primary" onClick={() => setUpdateOpen(true)}>อัปเดต</button>
          <button className="icon-btn" aria-label="ซ่อนการแจ้งเตือนอัปเดต" onClick={() => setUpdateDismissed(true)}><X size={16} strokeWidth={1.8} /></button>
        </div>
      )}
      {updateOpen && update && <UpdateDialog info={update} onClose={() => setUpdateOpen(false)} />}
      {dups.length > 0 && (() => {
        const d = dups[0];
        const best = d.similar[0];
        return (
          <div className="banner info" role="status">
            <span>
              มีบันทึกเรื่องนี้อยู่แล้ว: <b>{best.title}</b> (คล้าย {Math.round(best.similarity * 100)}%{d.similar.length > 1 ? ` · และอีก ${d.similar.length - 1} บันทึก` : ""})
              {dups.length > 1 ? ` · รออีก ${dups.length - 1} รายการ` : ""}
            </span>
            <button className="btn btn-primary" onClick={() => setComparing({ dup: d, other: best })}>ดูเทียบ</button>
            <button className="icon-btn" aria-label="ปิดการแจ้งเตือนบันทึกซ้ำ" onClick={() => dropDup(d.path)}><X size={16} strokeWidth={1.8} /></button>
          </div>
        );
      })()}
      {comparing && (
        <KnowledgeCompareDialog dup={comparing.dup} other={comparing.other} onClose={(done) => {
          const { dup, other } = comparing;
          setComparing(null);
          if (!done) return;
          // the next similar note of the same new note, if any (unless the new one was merged away)
          setDups((xs) => xs.flatMap((x) => {
            if (x.path !== dup.path) return [x];
            const rest = x.similar.filter((s) => s.path !== other.path);
            return rest.length ? [{ ...x, similar: rest }] : [];
          }));
        }} />
      )}
      <ConnectDialog />
      <div className="app" style={{ flex: 1, minHeight: 0 }}>
        <Sidebar open={menuOpen} onNavigate={() => setMenuOpen(false)} />
        {menuOpen && <div className="modal-backdrop only-narrow" style={{ zIndex: 44, background: "var(--overlay)" }} onClick={() => setMenuOpen(false)} aria-hidden="true" />}
        {page.name === "chat" && <ChatPage chatId={page.chatId} initial={page.initial} onMenu={() => setMenuOpen(true)} />}
        {page.name === "newchat" && <NewChatPage onMenu={() => setMenuOpen(true)} />}
        {page.name === "general" && <NewChatPage general onMenu={() => setMenuOpen(true)} />}
        {page.name === "code" && <NewChatPage coding onMenu={() => setMenuOpen(true)} />}
        {page.name === "services" && <ServicesPage />}
        {page.name === "settings" && <SettingsPage section={page.section} />}
      </div>
      {api.mock && <span className="mock-badge">{th.mock}</span>}
    </div>
  );
}
