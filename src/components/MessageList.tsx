import { useEffect, useState } from "react";
import { BookMarked, Copy, Check, RotateCcw } from "lucide-react";
import type { Message, Source } from "../lib/types";
import { th } from "../i18n/th";
import { initials, useApp } from "../state";
import { Markdown } from "./Markdown";
import { ErrorState } from "./ui";

export interface StreamingState {
  content: string;
  sources: Source[];
  error: string | null;
}

export function MessageList({
  messages, streaming, onRetry, retryDisabled, onSaveKnowledge,
}: {
  messages: Message[]; streaming: StreamingState | null; onRetry: (question: string) => void; retryDisabled: boolean;
  /** "บันทึกเป็นความรู้" from this answer back to the start of the chat */
  onSaveKnowledge?: (messageId: number) => void;
}) {
  const { settings } = useApp();
  const me = initials(settings.userName || "ผู้ใช้");
  return (
    <div className="messages" aria-live="polite">
      {messages.map((m, i) =>
        m.role === "user" ? (
          <UserMsg key={m.id} text={m.content} avatar={me} />
        ) : (
          <AiMsg
            key={m.id}
            msg={m}
            retryDisabled={retryDisabled}
            onSaveKnowledge={onSaveKnowledge && !m.error && m.content ? () => onSaveKnowledge(m.id) : undefined}
            onRetry={() => {
              const q = [...messages.slice(0, i)].reverse().find((x) => x.role === "user");
              if (q) onRetry(q.content);
            }}
          />
        ),
      )}
      {streaming && (
        <div className="ai-msg" aria-busy={!streaming.error}>
          {streaming.content ? (
            <Markdown text={streaming.content} sources={streaming.sources} trailing={<span className="caret" aria-hidden="true" />} />
          ) : !streaming.error ? (
            <div className="ai-text muted" style={{ fontSize: 14 }}>
              {streaming.sources.length ? `พบ ${streaming.sources.length} ท่อนที่เกี่ยวข้อง กำลังถาม Gateway…` : "กำลังค้นเอกสาร…"}
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

// sources are reached through the numbered citations inside the answer only
function AiMsg({ msg, onRetry, retryDisabled, onSaveKnowledge }: { msg: Message; onRetry: () => void; retryDisabled: boolean; onSaveKnowledge?: () => void }) {
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
