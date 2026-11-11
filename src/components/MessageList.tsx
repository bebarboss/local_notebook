import { useState } from "react";
import { Copy, Check, RotateCcw } from "lucide-react";
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
  messages, streaming, onRetry, retryDisabled,
}: {
  messages: Message[]; streaming: StreamingState | null; onRetry: (question: string) => void; showServiceInSources: boolean; retryDisabled: boolean;
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
function AiMsg({ msg, onRetry, retryDisabled }: { msg: Message; onRetry: () => void; retryDisabled: boolean }) {
  const [copied, setCopied] = useState(false);
  const info = [msg.model, msg.reasoningEffort, msg.elapsedMs != null ? th.seconds(msg.elapsedMs) : null].filter(Boolean).join(" · ");
  return (
    <div className="ai-msg">
      {msg.content && <Markdown text={msg.content} sources={msg.sources} />}
      {msg.error && <ErrorState message={msg.error} />}
      <div className="ai-meta">
        <button
          className="icon-btn"
          aria-label={copied ? th.copied : th.copy}
          title={copied ? th.copied : th.copy}
          onClick={async () => {
            // copies the answer text only — never anything secret
            await navigator.clipboard.writeText(msg.content);
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          }}
        >
          {copied ? <Check size={16} strokeWidth={1.8} /> : <Copy size={16} strokeWidth={1.8} />}
        </button>
        <button className="icon-btn" aria-label={th.retry} title={th.retry} onClick={onRetry} disabled={retryDisabled}>
          <RotateCcw size={16} strokeWidth={1.8} />
        </button>
        {info && <span className="info">{info}</span>}
      </div>
    </div>
  );
}
