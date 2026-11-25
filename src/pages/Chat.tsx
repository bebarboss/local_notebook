import { useCallback, useEffect, useRef, useState } from "react";
import { Code2, FileText, Menu, MessageCircle } from "lucide-react";
import { api } from "../lib/api";
import type { AskRequest, Chat as ChatT, Effort, Message } from "../lib/types";
import { th } from "../i18n/th";
import { useApp, visibleModels } from "../state";
import { Composer } from "../components/Composer";
import { DocPanel } from "../components/DocPanel";
import { MessageList, type StreamingState } from "../components/MessageList";
import { KnowledgeDialog } from "../components/KnowledgeDialog";
import { ErrorState, Skeleton } from "../components/ui";

export default function ChatPage({ chatId, initial, onMenu }: { chatId: string; initial?: AskRequest; onMenu: () => void }) {
  const app = useApp();
  const { services, stats, gateway, go, models: allModels, setActiveServiceId, setAppMode, bumpChats, live, startLive, failLive, clearLive } = app;
  const models = visibleModels(allModels);
  const [chat, setChat] = useState<ChatT | null>(null);
  const [messages, setMessages] = useState<Message[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  // a synchronous failure of ask() itself (before any job/event), e.g. gateway down on a brand-new chat
  const [askError, setAskError] = useState<string | null>(null);
  const [showDocs, setShowDocs] = useState(() => window.innerWidth > 1100);
  const [model, setModel] = useState(initial?.model ?? "");
  const [effort, setEffort] = useState<Effort>(initial?.effort ?? "medium");
  const [allServices, setAllServices] = useState(initial?.allServices ?? false);
  const [knowledgeUpto, setKnowledgeUpto] = useState<number | null>(null);
  const [web, setWeb] = useState(initial?.web ?? false);
  const [projectDir, setProjectDir] = useState<string | null>(initial?.projectDir ?? null);
  const scroller = useRef<HTMLDivElement>(null);
  const started = useRef(false);
  // the chat id this page just created via send() → don't wipe its state on the "new"→id transition
  const justCreated = useRef<string | null>(null);
  const liveRef = useRef(live);
  liveRef.current = live;

  const serviceId = chat?.serviceId ?? initial?.serviceId ?? null;
  const service = services.find((s) => s.id === serviceId) ?? null;
  const mode = chat?.mode ?? initial?.mode ?? "docs";
  const docsMode = mode === "docs";
  const codeMode = mode === "code";
  // folder: a re-pick in this session wins; otherwise the one persisted on the chat (or from `initial`)
  const projectFolder = projectDir ?? chat?.projectDir ?? null;

  // the in-flight generation for this chat is kept at app level, so it survives leaving/returning
  const liveStream = chatId !== "new" ? live[chatId] : undefined;
  const activeStream = liveStream && !liveStream.done ? liveStream : undefined;
  const stream: StreamingState | null = activeStream ?? (askError ? { content: "", sources: [], error: askError } : null);

  // keep the sidebar workspace in sync with the chat being viewed
  useEffect(() => { setAppMode(mode); }, [mode, setAppMode]);

  // ---- load existing chat (from SQLite, no Gateway call)
  const load = useCallback(async () => {
    if (chatId === "new") return;
    setLoadError(null);
    setAskError(null);
    // a turn that finished (done/errored) while we were away is already saved to DB — drop its live
    const l = liveRef.current[chatId];
    if (l && (l.done || l.error)) clearLive(chatId);
    try {
      const [c, m] = await Promise.all([api.getChat(chatId), api.getMessages(chatId)]);
      setChat(c);
      setMessages(m);
      setActiveServiceId(c.serviceId);
      const lastAi = [...m].reverse().find((x) => x.role === "assistant" && x.model);
      const svc = services.find((s) => s.id === c.serviceId);
      setModel((cur) => cur || lastAi?.model || svc?.model || "");
      setEffort((lastAi?.reasoningEffort as Effort) || svc?.effort || "medium");
    } catch (e) {
      setLoadError((e as { message: string }).message);
    }
  }, [chatId, services, setActiveServiceId, clearLive]);

  const shownChat = useRef<string | null>(null);
  useEffect(() => {
    // only reset when the chat really changes (StrictMode re-runs effects)
    if (shownChat.current === chatId) return;
    shownChat.current = chatId;
    if (chatId !== "new" && justCreated.current === chatId) { justCreated.current = null; return; } // just created by us
    setMessages(chatId === "new" ? [] : null);
    setChat(null);
    setAskError(null);
    setModel(initial?.model ?? "");
    setAllServices(initial?.allServices ?? false);
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chatId]);

  // when the running turn finishes, fold its saved message into the list and drop the live stream
  useEffect(() => {
    if (chatId === "new") return;
    const l = live[chatId];
    if (l?.done) {
      const msg = l.done;
      setMessages((m) => ((m ?? []).some((x) => x.id === msg.id) ? (m ?? []) : [...(m ?? []), msg]));
      clearLive(chatId);
    }
  }, [live, chatId, clearLive]);

  // keep scrolled to bottom while streaming
  useEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, stream?.content, stream?.error]);

  const send = useCallback(
    async (question: string, override?: Partial<AskRequest>) => {
      if (docsMode && !serviceId) return;
      // requestId known before the call, so no early delta/done event can be missed
      const requestId = crypto.randomUUID();
      const existingId = chat?.id ?? (chatId === "new" ? null : chatId);
      const req: AskRequest = {
        chatId: existingId,
        serviceId, question, model, effort, mode, allServices,
        web: mode === "chat" ? web : false, code: codeMode, projectDir: codeMode ? projectFolder : null, ...override,
        requestId,
      };
      setAskError(null);
      if (existingId) startLive(existingId, requestId); // show "thinking" right away for an existing chat
      try {
        const r = await api.ask(req);
        setMessages((m) => [...(m ?? []), r.userMessage]);
        if (!existingId) {
          startLive(r.chatId, requestId);
          justCreated.current = r.chatId; // the reset effect must not wipe this freshly-created chat
          const c = await api.getChat(r.chatId).catch(() => null);
          if (c) setChat(c);
          bumpChats();
          go({ name: "chat", chatId: r.chatId });
        }
      } catch (e) {
        const err = e as { code: string; message: string };
        if (existingId) failLive(existingId, err.message);
        else setAskError(err.message);
        if (err.code === "gateway_unreachable") app.recheckGateway();
      }
    },
    [serviceId, docsMode, codeMode, mode, chat, chatId, model, effort, allServices, web, projectFolder, bumpChats, go, app, startLive, failLive],
  );

  // first question coming from NewChat
  useEffect(() => {
    if (initial && chatId === "new" && !started.current) {
      started.current = true;
      // defer to the next tick so the reset above has committed first
      setTimeout(() => send(initial.question, initial), 0);
    }
  }, [initial, chatId, send]);

  const sendBlocked =
    gateway.state === "unreachable" ? th.err.unreachable :
    gateway.state === "error" ? gateway.message ?? th.gatewayError :
    !models.length ? th.err.noModels : null;

  const addDocs = async () => {
    if (!service) return;
    const files = await api.pickFiles();
    if (files.length) {
      await api.addDocuments(service.id, files);
      app.toast(`เพิ่ม ${files.length} ไฟล์ใน ${service.code} กำลังทำ index`);
      app.refreshServices();
      setShowDocs(true);
    }
  };

  const title = chat?.title ?? initial?.question.slice(0, 40) ?? "";

  return (
    <>
      <main className="main" aria-label="แชต">
        <div className="topbar">
          <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0, flex: 1 }}>
            <button className="icon-btn only-narrow" aria-label="เปิดเมนู" onClick={onMenu}><Menu size={18} strokeWidth={1.8} /></button>
            {codeMode && (
              <span className="chip chip-general" style={{ flex: "none" }} title={projectFolder ?? undefined}>
                <Code2 size={13} strokeWidth={2} aria-hidden="true" />
                เขียนโค้ด
              </span>
            )}
            {!docsMode && !codeMode && (
              <span className="chip chip-general" style={{ flex: "none" }}>
                <MessageCircle size={13} strokeWidth={2} aria-hidden="true" />
                คุยทั่วไป
              </span>
            )}
            {docsMode && service && (
              <span className="chip" style={{ flex: "none" }}>
                <span className="swatch" style={{ width: 8, height: 8, borderRadius: 2, background: service.color }} aria-hidden="true" />
                {service.name}
              </span>
            )}
            <span className="muted" aria-hidden="true">/</span>
            {title ? <h1 className="ellipsis" style={{ fontSize: 14, fontWeight: 600, color: "var(--text-2)" }}>{title}</h1> : <Skeleton w={160} />}
          </div>
          {service && (
            <button className="btn" aria-pressed={showDocs} onClick={() => setShowDocs((v) => !v)} style={{ fontWeight: 500, color: "var(--text-2)" }}>
              <FileText size={16} strokeWidth={1.8} aria-hidden="true" />
              {th.docs(stats[service.id]?.documents ?? 0)}
            </button>
          )}
        </div>

        <div className="scroll" ref={scroller}>
          <div className="column">
            {loadError && <ErrorState message={loadError} onRetry={load} />}
            {!messages && !loadError && (
              <div style={{ display: "flex", flexDirection: "column", gap: 16 }} aria-busy="true">
                <Skeleton h={44} w="60%" style={{ borderRadius: 16 }} />
                <Skeleton h={16} /><Skeleton h={16} w="90%" /><Skeleton h={16} w="70%" />
              </div>
            )}
            {messages && (
              <MessageList
                messages={messages}
                streaming={stream}
                retryDisabled={!!stream && !stream.error}
                onRetry={(q) => send(q)}
                onSaveKnowledge={docsMode && chat && !(stream && !stream.error) ? (id) => setKnowledgeUpto(id) : undefined}
                onResolvePermission={stream?.permission ? (allow) => app.answerPermission(chatId, stream.permission!.requestId, allow) : undefined}
              />
            )}
            {stream?.error && (
              <div style={{ marginTop: -8, marginBottom: 12 }}>
                <button className="btn" onClick={() => {
                  const q = [...(messages ?? [])].reverse().find((m) => m.role === "user");
                  if (q) { setMessages((m) => (m ?? []).filter((x) => x.id !== q.id)); send(q.content); }
                }}>{th.tryAgain}</button>
              </div>
            )}
          </div>
        </div>

        <Composer
          service={service}
          docsMode={docsMode}
          models={models}
          model={model}
          effort={effort}
          onModelChange={(m, e) => { setModel(m); setEffort(e); }}
          allServices={allServices}
          onToggleAll={() => setAllServices((v) => !v)}
          onSend={(q, atts) => send(q, atts && atts.length ? { attachments: atts } : undefined)}
          onStop={() => activeStream?.requestId && api.stop(activeStream.requestId)}
          onAddDocs={docsMode ? addDocs : undefined}
          onPickFiles={api.pickFiles}
          onSavePastedImage={api.savePastedImage}
          web={web}
          onToggleWeb={mode === "chat" ? () => setWeb((v) => !v) : undefined}
          code={codeMode ? true : undefined}
          projectDir={codeMode ? projectFolder : undefined}
          onPickProjectDir={codeMode ? async () => { const d = await api.pickFolder(); if (d) setProjectDir(d); } : undefined}
          onOpenProject={codeMode && projectFolder ? (target) => { (target === "editor" ? api.openInEditor(projectFolder) : api.openProjectFolder(projectFolder)).catch((e) => app.toast((e as { message?: string }).message ?? "เปิดไม่ได้")); } : undefined}
          streaming={!!stream && !stream.error}
          sendBlockedReason={sendBlocked}
          placeholder={codeMode ? "บอกงานที่จะให้แก้โค้ด…" : undefined}
          hint={codeMode ? undefined : docsMode ? undefined : "คุยทั่วไป — ไม่ค้นเอกสาร ไม่ผูก service"}
          autoFocus
        />
      </main>
      {service && showDocs && <DocPanel service={service} onClose={() => setShowDocs(false)} />}
      {knowledgeUpto != null && chat && chat.serviceId && (
        <KnowledgeDialog chatId={chat.id} serviceId={chat.serviceId} upto={knowledgeUpto} onClose={() => setKnowledgeUpto(null)} />
      )}
    </>
  );
}
