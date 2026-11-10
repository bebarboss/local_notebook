import { useCallback, useEffect, useRef, useState } from "react";
import { FileText, Menu } from "lucide-react";
import { api } from "../lib/api";
import type { AskRequest, Chat as ChatT, Effort, Message } from "../lib/types";
import { th } from "../i18n/th";
import { useApp, visibleModels } from "../state";
import { Composer } from "../components/Composer";
import { DocPanel } from "../components/DocPanel";
import { MessageList, type StreamingState } from "../components/MessageList";
import { ErrorState, Skeleton } from "../components/ui";

interface Stream extends StreamingState { requestId: string | null; chatId: string | null }

export default function ChatPage({ chatId, initial, onMenu }: { chatId: string; initial?: AskRequest; onMenu: () => void }) {
  const app = useApp();
  const { services, stats, gateway, go, models: allModels, setActiveServiceId, bumpChats } = app;
  const models = visibleModels(allModels);
  const [chat, setChat] = useState<ChatT | null>(null);
  const [messages, setMessages] = useState<Message[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [stream, setStream] = useState<Stream | null>(null);
  const [showDocs, setShowDocs] = useState(() => window.innerWidth > 1100);
  const [model, setModel] = useState(initial?.model ?? "");
  const [effort, setEffort] = useState<Effort>(initial?.effort ?? "medium");
  const [allServices, setAllServices] = useState(initial?.allServices ?? false);
  const scroller = useRef<HTMLDivElement>(null);
  const started = useRef(false);
  const streamRef = useRef<Stream | null>(null);
  streamRef.current = stream;

  const serviceId = chat?.serviceId ?? initial?.serviceId ?? null;
  const service = services.find((s) => s.id === serviceId) ?? null;

  // ---- load existing chat (from SQLite, no Gateway call)
  const load = useCallback(async () => {
    if (chatId === "new") return;
    setLoadError(null);
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
  }, [chatId, services, setActiveServiceId]);

  const shownChat = useRef<string | null>(null);
  useEffect(() => {
    // only reset when the chat really changes (StrictMode re-runs effects)
    if (shownChat.current === chatId) return;
    shownChat.current = chatId;
    if (chatId !== "new" && streamRef.current?.chatId === chatId) return; // just created by us
    setMessages(chatId === "new" ? [] : null);
    setChat(null);
    setStream(null);
    setModel(initial?.model ?? "");
    setAllServices(initial?.allServices ?? false);
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chatId]);

  // ---- streaming events
  // Subscribe ONCE. Re-subscribing on every context change (e.g. a gateway-status
  // event) leaves a gap in which chat-done can be lost and the UI stays "streaming".
  const appRef = useRef(app);
  appRef.current = app;
  useEffect(() => {
    const offs = [
      api.on("chat-sources", (p) => setStream((s) => (s && s.requestId === p.requestId ? { ...s, sources: p.sources } : s))),
      api.on("chat-delta", (p) => setStream((s) => (s && s.requestId === p.requestId ? { ...s, content: s.content + p.delta } : s))),
      api.on("chat-done", (p) => {
        if (streamRef.current?.requestId !== p.requestId) return;
        setMessages((m) => [...(m ?? []), p.message]);
        setStream(null);
        appRef.current.bumpChats();
      }),
      api.on("chat-error", (p) => {
        if (streamRef.current?.requestId !== p.requestId) return;
        setStream((s) => (s ? { ...s, error: p.error.message } : s));
        appRef.current.bumpChats();
        if (p.error.code === "gateway_unreachable") appRef.current.recheckGateway();
      }),
    ];
    return () => offs.forEach((o) => o.then((f) => f()));
  }, []);

  // keep scrolled to bottom while streaming
  useEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, stream?.content, stream?.error]);

  const send = useCallback(
    async (question: string, override?: Partial<AskRequest>) => {
      if (!serviceId) return;
      const req: AskRequest = {
        chatId: chat?.id ?? (chatId === "new" ? null : chatId),
        serviceId, question, model, effort, allServices, ...override,
        // known before the call, so no early delta/done event can be missed
        requestId: crypto.randomUUID(),
      };
      const fresh: Stream = { requestId: req.requestId!, chatId: req.chatId, content: "", sources: [], error: null };
      streamRef.current = fresh;
      setStream(fresh);
      try {
        const r = await api.ask(req);
        setStream((s) => (s && s.requestId === r.requestId ? { ...s, chatId: r.chatId } : s));
        setMessages((m) => [...(m ?? []), r.userMessage]);
        if (!req.chatId) {
          const c = await api.getChat(r.chatId).catch(() => null);
          if (c) setChat(c);
          streamRef.current = { ...(streamRef.current as Stream), chatId: r.chatId };
          bumpChats();
          go({ name: "chat", chatId: r.chatId });
        }
      } catch (e) {
        const err = e as { code: string; message: string };
        setStream({ requestId: null, chatId: req.chatId, content: "", sources: [], error: err.message });
        if (err.code === "gateway_unreachable") app.recheckGateway();
      }
    },
    [serviceId, chat, chatId, model, effort, allServices, bumpChats, go, app],
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
            {service && (
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
                showServiceInSources={allServices || messages.some((m) => m.sources.some((s) => s.serviceId !== serviceId))}
                retryDisabled={!!stream && !stream.error}
                onRetry={(q) => send(q)}
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
          models={models}
          model={model}
          effort={effort}
          onModelChange={(m, e) => { setModel(m); setEffort(e); }}
          allServices={allServices}
          onToggleAll={() => setAllServices((v) => !v)}
          onSend={(q) => send(q)}
          onStop={() => stream?.requestId && api.stop(stream.requestId)}
          onAddDocs={addDocs}
          streaming={!!stream && !stream.error}
          sendBlockedReason={sendBlocked}
          autoFocus
        />
      </main>
      {service && showDocs && <DocPanel service={service} onClose={() => setShowDocs(false)} />}
    </>
  );
}
