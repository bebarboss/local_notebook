import { useEffect, useRef, useState } from "react";
import { BookMarked, BookOpen, Code2, Copy, Check, Circle, CircleAlert, CircleDot, Download, FileDown, FileSymlink, Globe, RotateCcw, Search, SkipForward, Terminal } from "lucide-react";
import type { AgentStep, CodeChange, CodePermissionRequest, ExportFormat, Message, PlanStep, Source } from "../lib/types";
import { th } from "../i18n/th";
import { api } from "../lib/api";
import { initials, useApp } from "../state";
import { Markdown } from "./Markdown";
import { ChangesList } from "./ChangesList";
import { ErrorState } from "./ui";

export interface StreamingState {
  content: string;
  sources: Source[];
  error: string | null;
  /** a follow-up question was searched with this */
  searchQuery?: string;
  /** the agentic gather step's activity, appended live each round */
  steps?: AgentStep[];
  /** files the coding agent changed this turn (shown live before the answer saves) */
  changes?: CodeChange[];
  /** the coding agent's plan checklist, updated live */
  plan?: PlanStep[];
  /** a pending "allow reading this outside file?" prompt */
  permission?: CodePermissionRequest | null;
}

/** "ค้นด้วย: …" — how a follow-up question was understood for the search */
function SearchedAs({ query }: { query?: string }) {
  if (!query) return null;
  return (
    <div className="searched-as" title="คำถามต่อเนื่อง: แอปเขียนคำถามใหม่ให้ครบความหมายจากบทสนทนาก่อนค้นเอกสาร">
      <Search size={12} strokeWidth={2} aria-hidden="true" />
      <span>ค้นด้วย: {query}</span>
    </div>
  );
}

function stepText(s: AgentStep): string {
  switch (s.kind) {
    case "search":
      return `ค้นเพิ่ม: ${s.detail}${s.found ? ` — เจอ ${s.found} ท่อน` : " — ไม่พบท่อนใหม่"}`;
    case "read":
      return `อ่านรอบแหล่ง ${s.detail}`;
    case "websearch":
      return `ค้นเว็บ: ${s.detail}${s.found ? ` — เจอ ${s.found} แหล่ง` : ""}`;
    case "webread":
      return `อ่านหน้า: ${s.detail}`;
    case "codelist":
      return `ดูโฟลเดอร์: ${s.detail}`;
    case "coderead":
      return `อ่านไฟล์: ${s.detail}`;
    case "codegrep":
      return `ค้นโค้ด: ${s.detail}`;
    case "codewrite":
      return `เขียนไฟล์: ${s.detail}`;
    case "codeedit":
      return `แก้ไฟล์: ${s.detail}`;
    case "coderun":
      return `รันคำสั่ง: ${s.detail}${s.found ? " — ผ่าน" : " — มีข้อผิดพลาด"}`;
    case "codeblocked":
      return `ไม่อนุญาตให้รัน: ${s.detail}`;
    case "codeskip":
      return `ข้ามไฟล์ (ไม่ได้รับอนุญาต): ${s.detail}`;
    case "done":
      return "เสร็จแล้ว";
    case "error": {
      const hint = errorStepHint(s.detail);
      return hint ? `หยุดกลางคัน: ${hint}` : "ทำบางขั้นไม่สำเร็จ ใช้ผลที่มี";
    }
    default:
      return s.detail;
  }
}

/** Map an error step's code (from AppError.code) to a short Thai reason, or "" to keep the generic text. */
function errorStepHint(code: string): string {
  if (code.includes("unreachable")) return "ต่อ Gateway ไม่ได้ (ตรวจ VPN) แล้วลองใหม่";
  if (code.includes("429")) return "Gateway เกินโควตา/ความถี่ รอสักครู่แล้วลองใหม่";
  if (code.includes("401") || code.includes("403")) return "สิทธิ์หรือ Key มีปัญหา";
  if (code.includes("404")) return "Gateway URL หรือชื่อโมเดลไม่ถูกต้อง";
  if (code.includes("timeout")) return "Gateway ตอบช้าเกินกำหนด ลองใหม่";
  if (code.includes("http") || code.includes("network")) return "Gateway ขัดข้องชั่วคราว ลองใหม่อีกครั้ง";
  return "";
}

function stepIcon(kind: string) {
  const p = { size: 12, strokeWidth: 2, "aria-hidden": true } as const;
  if (kind === "read") return <BookOpen {...p} />;
  if (kind === "websearch" || kind === "webread") return <Globe {...p} />;
  if (kind === "coderun" || kind === "codeblocked") return <Terminal {...p} />;
  if (kind === "codeskip") return <SkipForward {...p} />;
  if (kind.startsWith("code")) return <Code2 {...p} />;
  if (kind === "done") return <Check {...p} />;
  if (kind === "error") return <CircleAlert {...p} />;
  return <Search {...p} />;
}

/** "กำลังทำอะไร" — the agentic gather step's live activity, like Claude's tool steps */
function AgentSteps({ steps }: { steps?: AgentStep[] }) {
  if (!steps || steps.length === 0) return null;
  return (
    <div className="agent-steps" aria-label="ขั้นตอนการค้นเพิ่มของ AI">
      {steps.map((s, i) => (
        <div key={i} className="agent-step">
          {stepIcon(s.kind)}
          <span>{stepText(s)}</span>
        </div>
      ))}
    </div>
  );
}

/** The coding agent's plan as a ticking checklist (pending ○ / running ◉ / done ✓), like Claude Code. */
function PlanChecklist({ steps }: { steps?: PlanStep[] }) {
  if (!steps || steps.length === 0) return null;
  const done = steps.filter((s) => s.status === "done").length;
  return (
    <div className="plan-list" aria-label="แผนงานของผู้ช่วย">
      <div className="plan-head">แผนงาน <span className="plan-count">{done}/{steps.length}</span></div>
      {steps.map((s, i) => (
        <div key={i} className={`plan-step ${s.status}`}>
          {s.status === "done" ? <Check size={13} strokeWidth={2.4} aria-hidden="true" />
            : s.status === "running" ? <CircleDot size={13} strokeWidth={2.2} aria-hidden="true" />
            : <Circle size={13} strokeWidth={2} aria-hidden="true" />}
          <span>{s.text}</span>
        </div>
      ))}
    </div>
  );
}

/** "Allow reading this outside file?" — the mid-loop permission prompt (read-only, outside the project). */
function PermissionPrompt({ req, onResolve }: { req: CodePermissionRequest; onResolve?: (allow: boolean) => void }) {
  return (
    <div className="perm-prompt" role="alertdialog" aria-label="ขออนุญาตอ่านไฟล์ภายนอก">
      <div className="perm-head"><FileSymlink size={15} strokeWidth={2} aria-hidden="true" />อนุญาตให้อ่านไฟล์นอกโฟลเดอร์โปรเจค?</div>
      <div className="perm-path mono" title={req.path}>{req.path}</div>
      <div className="perm-note">อ่านอย่างเดียว · อนุญาตแล้วจะจำไว้ทั้งแชตนี้</div>
      <div className="perm-actions">
        <button className="btn btn-primary" onClick={() => onResolve?.(true)}>อนุญาต</button>
        <button className="btn" onClick={() => onResolve?.(false)}><SkipForward size={14} strokeWidth={2} aria-hidden="true" />ข้าม</button>
      </div>
    </div>
  );
}

export function MessageList({
  messages, streaming, onRetry, retryDisabled, onSaveKnowledge, onResolvePermission,
}: {
  messages: Message[]; streaming: StreamingState | null; onRetry: (question: string) => void; retryDisabled: boolean;
  /** "บันทึกเป็นความรู้" from this answer back to the start of the chat */
  onSaveKnowledge?: (messageId: number) => void;
  /** answer the coding agent's "allow reading this outside file?" prompt */
  onResolvePermission?: (allow: boolean) => void;
}) {
  const { settings } = useApp();
  const me = initials(settings.userName || "ผู้ใช้");
  return (
    <div className="messages" aria-live="polite">
      {messages.map((m, i) => {
        if (m.role === "user") return <UserMsg key={m.id} text={m.content} avatar={me} />;
        const q = [...messages.slice(0, i)].reverse().find((x) => x.role === "user");
        return (
          <AiMsg
            key={m.id}
            msg={m}
            retryDisabled={retryDisabled}
            suggestedName={fileBase(q?.content)}
            onSaveKnowledge={onSaveKnowledge && !m.error && m.content ? () => onSaveKnowledge(m.id) : undefined}
            onRetry={() => {
              if (q) onRetry(q.content);
            }}
          />
        );
      })}
      {streaming && (
        <div className="ai-msg" aria-busy={!streaming.error}>
          <SearchedAs query={streaming.searchQuery} />
          <PlanChecklist steps={streaming.plan} />
          <AgentSteps steps={streaming.steps} />
          {streaming.permission && <PermissionPrompt req={streaming.permission} onResolve={onResolvePermission} />}
          {streaming.changes && <ChangesList changes={streaming.changes} />}
          {streaming.content ? (
            <Markdown text={streaming.content} sources={streaming.sources} trailing={<span className="caret" aria-hidden="true" />} />
          ) : !streaming.error ? (
            <div className="ai-text muted" style={{ fontSize: 14 }}>
              {streaming.sources.length
                ? `พบ ${streaming.sources.length} ท่อนที่เกี่ยวข้อง กำลังถาม Gateway…`
                : streaming.steps?.some((s) => s.kind.startsWith("code"))
                  ? "กำลังเขียนโค้ด…"
                  : "กำลังค้นเอกสาร…"}
              <span className="caret" aria-hidden="true" />
            </div>
          ) : null}
          {streaming.error && <ErrorState message={streaming.error} />}
        </div>
      )}
    </div>
  );
}

export function UserMsg({ text, avatar }: { text: string; avatar: string }) {
  return (
    <div className="user-msg">
      <span className="avatar sm" aria-hidden="true">{avatar}</span>
      <span className="text">{text}</span>
    </div>
  );
}

/** A safe, extension-less filename from the question that produced the answer. */
function fileBase(question?: string): string {
  const clean = (question ?? "").replace(/[\\/:*?"<>|\n\r\t]+/g, " ").replace(/\s+/g, " ").trim().slice(0, 50);
  return clean || "DocAsk-คำตอบ";
}

const EXPORT_FORMATS: { id: ExportFormat; label: string; hint: string }[] = [
  { id: "xlsx", label: "Excel (.xlsx)", hint: "ตาราง" },
  { id: "csv", label: "CSV (.csv)", hint: "ตาราง" },
  { id: "md", label: "Markdown (.md)", hint: "ข้อความเต็ม" },
  { id: "txt", label: "ข้อความ (.txt)", hint: "ข้อความล้วน" },
];

/** "บันทึกเป็นไฟล์": a Save dialog that writes the answer as md/txt/csv/xlsx. */
function ExportButton({ content, suggestedName }: { content: string; suggestedName: string }) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const wrap = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => { if (!wrap.current?.contains(e.target as Node)) setOpen(false); };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", onKey);
    return () => { document.removeEventListener("mousedown", close); document.removeEventListener("keydown", onKey); };
  }, [open]);
  const run = async (format: ExportFormat) => {
    setOpen(false); setBusy(true); setErr(null); setSaved(false);
    try {
      const path = await api.exportAnswer(content, format, suggestedName);
      if (path) { setSaved(true); setTimeout(() => setSaved(false), 3000); }
    } catch (e) {
      setErr((e as { message: string }).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="export-wrap" ref={wrap}>
      <button
        className="icon-btn"
        aria-label="บันทึกเป็นไฟล์"
        title="บันทึกคำตอบเป็นไฟล์ (Excel / CSV / Markdown / Text)"
        onClick={() => setOpen((v) => !v)}
        disabled={busy}
      >
        <Download size={16} strokeWidth={1.8} />
      </button>
      {open && (
        <div className="popover export-menu" role="menu" onMouseDown={(e) => e.stopPropagation()}>
          <div className="menu-label">บันทึกคำตอบเป็นไฟล์</div>
          {EXPORT_FORMATS.map((f) => (
            <button key={f.id} role="menuitem" className="menu-item" onClick={() => run(f.id)}>
              <FileDown size={14} strokeWidth={1.8} aria-hidden="true" />
              <span style={{ flex: 1 }}>{f.label}</span>
              <span className="export-hint">{f.hint}</span>
            </button>
          ))}
        </div>
      )}
      {saved && <span className="info">บันทึกแล้ว ✓</span>}
      {err && <span className="info" style={{ color: "var(--err-text)" }}>{err}</span>}
    </div>
  );
}

// sources are reached through the numbered citations inside the answer only
function AiMsg({ msg, onRetry, retryDisabled, onSaveKnowledge, suggestedName }: { msg: Message; onRetry: () => void; retryDisabled: boolean; onSaveKnowledge?: () => void; suggestedName: string }) {
  const [copied, setCopied] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const copy = async () => {
    // copies the answer text only — never anything secret
    await navigator.clipboard.writeText(msg.content);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };
  const info = [msg.model, msg.reasoningEffort, msg.elapsedMs != null ? th.seconds(msg.elapsedMs) : null].filter(Boolean).join(" · ");
  return (
    <div
      className="ai-msg"
      onContextMenu={(e) => {
        if (!onSaveKnowledge) return;
        // keep the browser menu when text is selected (copy a part of the answer)
        if (window.getSelection()?.toString()) return;
        e.preventDefault();
        setMenu({ x: Math.min(e.clientX, window.innerWidth - 230), y: Math.min(e.clientY, window.innerHeight - 110) });
      }}
    >
      {menu && <ContextMenu at={menu} onClose={() => setMenu(null)} onCopy={copy} onSaveKnowledge={onSaveKnowledge!} />}
      <SearchedAs query={msg.searchQuery} />
      <PlanChecklist steps={msg.plan} />
      <AgentSteps steps={msg.steps} />
      {msg.changes && msg.changes.items.length > 0 && <CodeChanges chatId={msg.chatId} messageId={msg.id} items={msg.changes.items} />}
      {msg.content && <Markdown text={msg.content} sources={msg.sources} />}
      {msg.error && <ErrorState message={msg.error} />}
      <div className="ai-meta">
        <button
          className="icon-btn"
          aria-label={copied ? th.copied : th.copy}
          title={copied ? th.copied : th.copy}
          onClick={copy}
        >
          {copied ? <Check size={16} strokeWidth={1.8} /> : <Copy size={16} strokeWidth={1.8} />}
        </button>
        {msg.content && !msg.error && <ExportButton content={msg.content} suggestedName={suggestedName} />}
        {onSaveKnowledge && (
          <button className="icon-btn" aria-label="บันทึกเป็นความรู้" title="บันทึกเป็นความรู้ (หรือคลิกขวาที่คำตอบ)" onClick={onSaveKnowledge}>
            <BookMarked size={16} strokeWidth={1.8} />
          </button>
        )}
        <button className="icon-btn" aria-label={th.retry} title={th.retry} onClick={onRetry} disabled={retryDisabled}>
          <RotateCcw size={16} strokeWidth={1.8} />
        </button>
        {info && <span className="info">{info}</span>}
      </div>
    </div>
  );
}

/** A saved coding turn's changes, with a working undo button. */
function CodeChanges({ chatId, messageId, items }: { chatId: string; messageId: number; items: CodeChange[] }) {
  const [undoing, setUndoing] = useState(false);
  const [undone, setUndone] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const undo = async () => {
    setUndoing(true);
    setErr(null);
    try {
      await api.undoCodeChanges(chatId, messageId);
      setUndone(true);
    } catch (e) {
      setErr((e as { message: string }).message);
    } finally {
      setUndoing(false);
    }
  };
  return (
    <>
      <ChangesList changes={items} onUndo={undo} undoing={undoing} undone={undone} />
      {err && <div className="ai-text muted" style={{ fontSize: 12.5, color: "var(--danger, #ef4444)" }}>ย้อนกลับไม่สำเร็จ: {err}</div>}
    </>
  );
}

function ContextMenu({ at, onClose, onCopy, onSaveKnowledge }: { at: { x: number; y: number }; onClose: () => void; onCopy: () => void; onSaveKnowledge: () => void }) {
  useEffect(() => {
    const close = () => onClose();
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    // the opening right-click is over by now; any later click or scroll closes the menu
    const t = setTimeout(() => {
      document.addEventListener("mousedown", close);
      document.addEventListener("contextmenu", close);
    });
    window.addEventListener("scroll", close, true);
    window.addEventListener("resize", close);
    document.addEventListener("keydown", onKey);
    return () => {
      clearTimeout(t);
      document.removeEventListener("mousedown", close);
      document.removeEventListener("contextmenu", close);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", close);
      document.removeEventListener("keydown", onKey);
    };
  }, [onClose]);
  const pick = (f: () => void) => (e: React.MouseEvent) => { e.stopPropagation(); onClose(); f(); };
  return (
    <div className="popover ctx-menu" role="menu" style={{ left: at.x, top: at.y }} onMouseDown={(e) => e.stopPropagation()}>
      <button role="menuitem" className="menu-item" autoFocus onClick={pick(onSaveKnowledge)}>
        <BookMarked size={14} strokeWidth={1.8} aria-hidden="true" />บันทึกเป็นความรู้…
      </button>
      <button role="menuitem" className="menu-item" onClick={pick(onCopy)}>
        <Copy size={14} strokeWidth={1.8} aria-hidden="true" />{th.copy}
      </button>
    </div>
  );
}
