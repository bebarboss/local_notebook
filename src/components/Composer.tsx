import { type ClipboardEvent, useEffect, useRef, useState } from "react";
import { ArrowUp, Code2, ExternalLink, FolderOpen, Globe, Paperclip, Plus, Square, X } from "lucide-react";
import type { Effort, ModelConfig, Service } from "../lib/types";
import { th } from "../i18n/th";
import { ModelPicker } from "./ModelPicker";

const baseName = (p: string) => p.replace(/[/\\]+$/, "").split(/[/\\]/).pop() || p;

export function Composer({
  service, docsMode = true, models, model, effort, onModelChange, allServices, onToggleAll, onSend, onStop, onAddDocs, onPickFiles, onSavePastedImage, web, onToggleWeb,
  code, onToggleCode, projectDir, onPickProjectDir, onOpenProject,
  streaming, sendBlockedReason, placeholder, hint, autoFocus, lockedScope,
}: {
  service: Service | null;
  /** docs chat (true) shows the scope + add-docs controls; general chat (false) hides them */
  docsMode?: boolean;
  models: ModelConfig[];
  model: string;
  effort: Effort;
  onModelChange: (m: string, e: Effort) => void;
  allServices: boolean;
  onToggleAll: () => void;
  onSend: (text: string, attachments?: string[]) => void;
  onStop: () => void;
  onAddDocs?: () => void;
  /** general chat: pick files/images to attach to this one question (returns their paths) */
  onPickFiles?: () => Promise<string[]>;
  /** general chat: save an image pasted (Ctrl+V) into the box; returns its temp path */
  onSavePastedImage?: (bytes: Uint8Array, ext: string) => Promise<string>;
  /** general chat: whether the "ค้นเว็บ" (web research) toggle is on */
  web?: boolean;
  /** general chat: toggle web research; the button shows only when this is provided */
  onToggleWeb?: () => void;
  /** general chat: whether the "เขียนโค้ด" (coding) toggle is on */
  code?: boolean;
  /** general chat: toggle coding mode; the button shows only when this is provided */
  onToggleCode?: () => void;
  /** general chat: the chosen project folder for coding mode (null = none yet) */
  projectDir?: string | null;
  /** general chat: pick/replace the project folder for coding mode */
  onPickProjectDir?: () => void;
  /** general chat: open the chosen project folder in the file manager ("folder") or VS Code ("editor") */
  onOpenProject?: (target: "folder" | "editor") => void;
  streaming: boolean;
  /** when set, typing works but sending is blocked (e.g. gateway down) */
  sendBlockedReason: string | null;
  placeholder?: string;
  hint?: string;
  autoFocus?: boolean;
  lockedScope?: boolean;
}) {
  const [text, setText] = useState("");
  const [attached, setAttached] = useState<string[]>([]);
  const [picking, setPicking] = useState(false);
  const ta = useRef<HTMLTextAreaElement>(null);
  // a general chat has no service but is still usable
  const disabled = docsMode && !service;

  useEffect(() => {
    if (autoFocus && !disabled) ta.current?.focus();
  }, [autoFocus, disabled]);

  // auto-grow
  useEffect(() => {
    const el = ta.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 220)}px`;
  }, [text]);

  // coding mode needs a project folder before it can send
  const blockReason = sendBlockedReason ?? (code && !projectDir ? "เลือกโฟลเดอร์โปรเจคก่อนใช้โหมดเขียนโค้ด" : null);
  // an attachment on its own is enough to search (its text/OCR becomes the query) — no typing needed
  const canSend = !disabled && !streaming && !blockReason && (text.trim().length > 0 || attached.length > 0) && !!model;
  const submit = () => {
    if (!canSend) return;
    onSend(text.trim(), attached.length ? attached : undefined);
    setText("");
    setAttached([]);
  };

  const pickFiles = async () => {
    if (!onPickFiles || picking) return;
    setPicking(true);
    try {
      const picked = await onPickFiles();
      if (picked.length) setAttached((a) => [...new Set([...a, ...picked])]);
    } finally {
      setPicking(false);
    }
  };

  const onPaste = async (e: ClipboardEvent<HTMLTextAreaElement>) => {
    if (!onSavePastedImage) return;
    const imgs = Array.from(e.clipboardData.items).filter((it) => it.kind === "file" && it.type.startsWith("image/"));
    if (!imgs.length) return;
    e.preventDefault();
    for (const it of imgs) {
      const file = it.getAsFile();
      if (!file) continue;
      const ext = (file.type.split("/")[1] || "png").replace("jpeg", "jpg").replace("svg+xml", "png");
      try {
        const bytes = new Uint8Array(await file.arrayBuffer());
        const path = await onSavePastedImage(bytes, ext);
        setAttached((a) => [...new Set([...a, path])]);
      } catch {
        /* a failed paste just does nothing — the user can try the + button */
      }
    }
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
            onPaste={onPaste}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                submit();
              }
            }}
          />
        </label>
        {attached.length > 0 && (
          <div className="attach-chips">
            {attached.map((p) => (
              <span key={p} className="attach-chip" title={p}>
                <Paperclip size={12} strokeWidth={2} aria-hidden="true" />
                <span className="ellipsis">{baseName(p)}</span>
                <button type="button" className="attach-x" aria-label={`เอา ${baseName(p)} ออก`} onClick={() => setAttached((a) => a.filter((x) => x !== p))}>
                  <X size={12} strokeWidth={2.4} />
                </button>
              </span>
            ))}
          </div>
        )}
        <div className="composer-row">
          {onPickFiles && (
            <button type="button" className="icon-btn bordered" aria-label="แนบไฟล์หรือรูป" title="แนบไฟล์หรือรูปเฉพาะคำถามนี้ (อ่านสด/OCR ไม่บันทึกเข้า service)" onClick={pickFiles} disabled={picking}>
              <Paperclip size={16} strokeWidth={2} />
            </button>
          )}
          {!docsMode && onToggleWeb && (
            <button type="button" className="scope-btn" aria-pressed={!!web} onClick={onToggleWeb} title="ค้นเว็บสาธารณะด้วย DuckDuckGo ก่อนตอบ (คำค้นถูกส่งออกอินเทอร์เน็ต)">
              <Globe size={14} strokeWidth={2} aria-hidden="true" />
              ค้นเว็บ
            </button>
          )}
          {!docsMode && onToggleCode && (
            <button type="button" className="scope-btn" aria-pressed={!!code} onClick={onToggleCode} title="ให้ผู้ช่วยอ่าน/แก้ไฟล์ในโฟลเดอร์โปรเจคที่เลือก (เขียนไฟล์จริง มีปุ่มย้อนกลับ)">
              <Code2 size={14} strokeWidth={2} aria-hidden="true" />
              เขียนโค้ด
            </button>
          )}
          {!docsMode && code && onPickProjectDir && (
            <button type="button" className="scope-btn" aria-pressed={!!projectDir} onClick={onPickProjectDir} title={projectDir ?? "เลือกโฟลเดอร์โปรเจคที่จะให้แก้ไฟล์"}>
              <FolderOpen size={14} strokeWidth={2} aria-hidden="true" />
              <span className="ellipsis" style={{ maxWidth: 160 }}>{projectDir ? baseName(projectDir) : "เลือกโฟลเดอร์"}</span>
            </button>
          )}
          {!docsMode && code && projectDir && onOpenProject && (
            <>
              <button type="button" className="icon-btn bordered" aria-label="เปิดโฟลเดอร์โปรเจคในตัวจัดการไฟล์" title="เปิดโฟลเดอร์โปรเจคในตัวจัดการไฟล์" onClick={() => onOpenProject("folder")}>
                <ExternalLink size={15} strokeWidth={2} />
              </button>
              <button type="button" className="icon-btn bordered" aria-label="เปิดโปรเจคใน VS Code" title="เปิดโปรเจคใน VS Code (ต้องมีคำสั่ง code ใน PATH)" onClick={() => onOpenProject("editor")}>
                <Code2 size={15} strokeWidth={2} />
              </button>
            </>
          )}
          {docsMode && onAddDocs && (
            <button type="button" className="icon-btn bordered" aria-label={th.addDocs} title={th.addDocs} onClick={onAddDocs} disabled={disabled}>
              <Plus size={16} strokeWidth={2} />
            </button>
          )}
          {docsMode && (
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
          )}
          <span className="spacer" />
          <ModelPicker models={models} model={model} effort={effort} onChange={onModelChange} disabled={disabled} />
          {streaming ? (
            <button type="button" className="send-btn stop" aria-label={th.stop} title={th.stop} onClick={onStop}>
              <Square size={14} strokeWidth={2.2} fill="currentColor" />
            </button>
          ) : (
            <button type="submit" className="send-btn" aria-label={th.send} title={blockReason ?? th.send} disabled={!canSend}>
              <ArrowUp size={16} strokeWidth={2.2} />
            </button>
          )}
        </div>
      </form>
      <span className="composer-hint">{blockReason ?? (!docsMode && code ? "โหมดเขียนโค้ด — ผู้ช่วยแก้ไฟล์ในโฟลเดอร์ที่เลือกโดยตรง (มีปุ่มย้อนกลับ)" : !docsMode && web ? "ค้นเว็บสาธารณะ — คำค้นถูกส่งออกอินเทอร์เน็ต (DuckDuckGo)" : hint ?? defaultHint)}</span>
    </div>
  );
}
