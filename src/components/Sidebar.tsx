import { useEffect, useState } from "react";
import { MoreHorizontal, Plus, Search, Settings as SettingsIcon, Pencil, Trash2 } from "lucide-react";
import { api } from "../lib/api";
import type { Chat } from "../lib/types";
import { th } from "../i18n/th";
import { initials, useApp } from "../state";
import { StatusPill } from "./StatusPill";
import { ConfirmDialog, Logo, Modal, Skeleton, usePopover } from "./ui";

export function Sidebar({ open, onNavigate }: { open: boolean; onNavigate: () => void }) {
  const app = useApp();
  const { services, stats, page, go, activeServiceId, setActiveServiceId, settings, gateway, chatsVersion } = app;
  const active = services.find((s) => s.id === activeServiceId) ?? null;
  const [chats, setChats] = useState<Chat[] | null>(null);
  const [query, setQuery] = useState("");
  const [renaming, setRenaming] = useState<Chat | null>(null);
  const [deleting, setDeleting] = useState<Chat | null>(null);

  useEffect(() => {
    if (!active) { setChats([]); return; }
    let alive = true;
    setChats(null);
    const t = setTimeout(() => {
      api.listChats(active.id, query).then((c) => alive && setChats(c)).catch(() => alive && setChats([]));
    }, query ? 200 : 0);
    return () => { alive = false; clearTimeout(t); };
  }, [active, query, chatsVersion]);

  const openChat = page.name === "chat" ? page.chatId : null;

  const nav = (fn: () => void) => () => { fn(); onNavigate(); };

  return (
    <aside className={`sidebar${open ? " open" : ""}`} aria-label="เมนูหลัก">
      <Logo />
      <button
        className={`nav-item nav-new${page.name === "newchat" ? " selected" : ""}`}
        onClick={nav(() => go({ name: "newchat" }))}
      >
        <span className="plus" aria-hidden="true"><Plus size={14} strokeWidth={2.4} /></span>
        {th.newChat}
      </button>

      <div className="side-head">
        <span>{th.service}</span>
        <button className="link-btn" onClick={nav(() => go({ name: "services" }))}>{th.manage}</button>
      </div>
      {services.length === 0 && (
        <button className="nav-item" onClick={nav(() => go({ name: "services" }))} style={{ color: "var(--accent-text)" }}>
          <Plus size={16} strokeWidth={1.8} aria-hidden="true" /> สร้าง service แรก
        </button>
      )}
      {services.map((s) => (
        <button
          key={s.id}
          className={`nav-item${s.id === activeServiceId ? " selected" : ""}`}
          aria-current={s.id === activeServiceId ? "true" : undefined}
          onClick={() => {
            setActiveServiceId(s.id);
            if (page.name === "chat") { /* stay; list below changes */ }
          }}
        >
          <span className="swatch" style={{ background: s.color }} aria-hidden="true" />
          <span className="ellipsis" style={{ flex: 1 }}>{s.name}</span>
          <span style={{ fontSize: 12, color: s.id === activeServiceId ? undefined : "var(--muted)" }}>{stats[s.id]?.documents ?? 0}</span>
        </button>
      ))}

      {active && (
        <>
          <div className="side-head" style={{ paddingTop: 16 }}><span className="ellipsis">{th.recentIn(active.code)}</span></div>
          <label style={{ position: "relative", margin: "2px 0 4px" }}>
            <span className="sr-only">{th.searchChats}</span>
            <Search size={14} aria-hidden="true" style={{ position: "absolute", left: 10, top: 11, color: "var(--muted)" }} />
            <input className="input" style={{ minHeight: 36, padding: "6px 10px 6px 30px", fontSize: 13, background: "var(--surface)" }} placeholder={th.searchChats} value={query} onChange={(e) => setQuery(e.target.value)} />
          </label>
          {chats === null && <div style={{ display: "flex", flexDirection: "column", gap: 6, padding: "4px 10px" }}><Skeleton /><Skeleton w="80%" /><Skeleton w="60%" /></div>}
          {chats?.length === 0 && <span style={{ fontSize: 12, color: "var(--muted)", padding: "4px 10px" }}>{query ? "ไม่พบแชตที่ค้นหา" : th.noChats}</span>}
          {chats?.map((c) => (
            <ChatRow key={c.id} chat={c} open={c.id === openChat} onOpen={nav(() => go({ name: "chat", chatId: c.id }))} onRename={() => setRenaming(c)} onDelete={() => setDeleting(c)} />
          ))}
        </>
      )}

      <div className="side-foot">
        <span className="avatar" aria-hidden="true">{initials(settings.userName || "ผู้ใช้")}</span>
        <div style={{ display: "flex", flexDirection: "column", gap: 1, minWidth: 0, flex: 1 }}>
          <span className="ellipsis" style={{ fontSize: 13, fontWeight: 600 }}>{settings.userName || "ผู้ใช้"}</span>
          <StatusPill status={gateway} />
        </div>
        <button className="icon-btn" aria-label={th.settings} onClick={nav(() => go({ name: "settings" }))}>
          <SettingsIcon size={18} strokeWidth={1.8} />
        </button>
      </div>

      {renaming && <RenameDialog chat={renaming} onClose={() => setRenaming(null)} />}
      {deleting && (
        <ConfirmDialog
          title="ลบแชตนี้?"
          body={<>แชต "<b>{deleting.title}</b>" จะถูกลบออกจากเครื่องนี้ถาวร</>}
          confirmLabel={th.delete}
          danger
          onCancel={() => setDeleting(null)}
          onConfirm={async () => {
            await api.deleteChat(deleting.id);
            if (openChat === deleting.id) go({ name: "newchat" });
            setDeleting(null);
            app.bumpChats();
          }}
        />
      )}
    </aside>
  );
}

function ChatRow({ chat, open, onOpen, onRename, onDelete }: { chat: Chat; open: boolean; onOpen: () => void; onRename: () => void; onDelete: () => void }) {
  const pop = usePopover();
  return (
    <div className="chat-row" ref={pop.ref}>
      <button className={`nav-item${open ? " open" : ""}`} onClick={onOpen} aria-current={open ? "page" : undefined}>
        <span className="ellipsis">{chat.title}</span>
      </button>
      <button className="icon-btn row-menu" aria-label={`ตัวเลือกของแชต ${chat.title}`} aria-haspopup="menu" aria-expanded={pop.open} onClick={() => pop.setOpen(!pop.open)}>
        <MoreHorizontal size={16} strokeWidth={1.8} />
      </button>
      {pop.open && (
        <div className="popover" role="menu" style={{ right: 0, top: 36 }}>
          <button role="menuitem" className="menu-item" onClick={() => { pop.setOpen(false); onRename(); }}><Pencil size={14} strokeWidth={1.8} aria-hidden="true" />{th.rename}</button>
          <button role="menuitem" className="menu-item danger" onClick={() => { pop.setOpen(false); onDelete(); }}><Trash2 size={14} strokeWidth={1.8} aria-hidden="true" />{th.delete}</button>
        </div>
      )}
    </div>
  );
}

function RenameDialog({ chat, onClose }: { chat: Chat; onClose: () => void }) {
  const app = useApp();
  const [title, setTitle] = useState(chat.title);
  return (
    <Modal title="เปลี่ยนชื่อแชต" onClose={onClose} width={440}>
      <form
        style={{ display: "flex", flexDirection: "column", gap: 16 }}
        onSubmit={async (e) => {
          e.preventDefault();
          if (!title.trim()) return;
          await api.renameChat(chat.id, title.trim());
          app.bumpChats();
          onClose();
        }}
      >
        <div className="field">
          <label htmlFor="chat-title">ชื่อแชต</label>
          <input id="chat-title" className="input" value={title} maxLength={120} onChange={(e) => setTitle(e.target.value)} />
        </div>
        <div className="modal-actions">
          <button type="button" className="btn btn-lg" onClick={onClose}>{th.cancel}</button>
          <button type="submit" className="btn btn-lg btn-primary" disabled={!title.trim()}>{th.save}</button>
        </div>
      </form>
    </Modal>
  );
}
