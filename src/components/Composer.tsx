import { useEffect, useRef, useState } from "react";
import { ArrowUp, Plus, Square } from "lucide-react";
import type { Effort, ModelConfig, Service } from "../lib/types";
import { th } from "../i18n/th";
import { ModelPicker } from "./ModelPicker";

export function Composer({
  service, models, model, effort, onModelChange, allServices, onToggleAll, onSend, onStop, onAddDocs,
  streaming, sendBlockedReason, placeholder, hint, autoFocus, lockedScope,
}: {
  service: Service | null;
  models: ModelConfig[];
  model: string;
  effort: Effort;
  onModelChange: (m: string, e: Effort) => void;
  allServices: boolean;
  onToggleAll: () => void;
  onSend: (text: string) => void;
  onStop: () => void;
  onAddDocs?: () => void;
  streaming: boolean;
  /** when set, typing works but sending is blocked (e.g. gateway down) */
  sendBlockedReason: string | null;
  placeholder?: string;
  hint?: string;
  autoFocus?: boolean;
  lockedScope?: boolean;
}) {
  const [text, setText] = useState("");
  const ta = useRef<HTMLTextAreaElement>(null);
  const disabled = !service;

  useEffect(() => {
    if (autoFocus && service) ta.current?.focus();
  }, [autoFocus, service]);

  // auto-grow
  useEffect(() => {
    const el = ta.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 220)}px`;
  }, [text]);

  const canSend = !disabled && !streaming && !sendBlockedReason && text.trim().length > 0 && !!model;
  const submit = () => {
    if (!canSend) return;
    onSend(text.trim());
    setText("");
  };

  const defaultHint = service
    ? allServices
      ? `ค้นเอกสารทุก service แล้วส่งท่อนที่เกี่ยวข้องไปยัง Gateway บริษัท`
      : `ค้นเฉพาะเอกสารใน service ${service.code} แล้วส่ง ${service.topK} ท่อนที่เกี่ยวข้องไปยัง Gateway บริษัท`
    : "แชตแต่ละห้องผูกกับ service เดียว";

  return (
    <div className="composer-wrap">
      <form
        className={`composer${disabled ? " dim" : ""}`}
        onSubmit={(e) => { e.preventDefault(); submit(); }}
        aria-disabled={disabled}
      >
        <label style={{ display: "flex" }}>
          <span className="sr-only">ข้อความ</span>
          <textarea
            ref={ta}
            rows={2}
            value={text}
            disabled={disabled}
            placeholder={disabled ? th.composerPickFirst : placeholder ?? th.composerPlaceholder}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                submit();
              }
            }}
          />
        </label>
        <div className="composer-row">
          {onAddDocs && (
            <button type="button" className="icon-btn bordered" aria-label={th.addDocs} title={th.addDocs} onClick={onAddDocs} disabled={disabled}>
              <Plus size={16} strokeWidth={2} />
            </button>
          )}
          <button
            type="button"
            className="scope-btn"
            aria-pressed={allServices}
            disabled={disabled || lockedScope}
            onClick={onToggleAll}
            title="กดเพื่อสลับระหว่างค้นเฉพาะ service นี้ / ค้นทุก service"
          >
            <span className="swatch" style={{ width: 8, height: 8, borderRadius: 2, background: allServices ? "var(--accent)" : service?.color ?? "var(--disabled)" }} aria-hidden="true" />
            {service ? (allServices ? th.searchAll : th.searchIn(service.code)) : "ค้นใน: ยังไม่ได้เลือก"}
          </button>
          <span className="spacer" />
          <ModelPicker models={models} model={model} effort={effort} onChange={onModelChange} disabled={disabled} />
          {streaming ? (
            <button type="button" className="send-btn stop" aria-label={th.stop} title={th.stop} onClick={onStop}>
              <Square size={14} strokeWidth={2.2} fill="currentColor" />
            </button>
          ) : (
            <button type="submit" className="send-btn" aria-label={th.send} title={sendBlockedReason ?? th.send} disabled={!canSend}>
              <ArrowUp size={16} strokeWidth={2.2} />
            </button>
          )}
        </div>
      </form>
      <span className="composer-hint">{sendBlockedReason ?? hint ?? defaultHint}</span>
    </div>
  );
}
