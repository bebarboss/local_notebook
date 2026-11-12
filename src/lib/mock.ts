// In-memory mock backend used by `npm run dev` in a browser or DOCASK_MOCK=1.
// Never touches the network.

import type { Backend, EventMap, EventName, NewService } from "./api";
import type {
  Chat,
  DocItem,
  EmbeddingStatus,
  GatewayInfo,
  GatewayStatus,
  Message,
  ModelConfig,
  Service,
  RerankStatus,
  ServiceStats,
  Settings,
  Source,
  TestResult,
} from "./types";
import { defaultModels, DEFAULT_GATEWAY_URL, errorText } from "./gateway";

const now = () => Date.now();
const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v));
let seq = 1000;
const uid = (p: string) => `${p}-${(++seq).toString(36)}-${Math.random().toString(36).slice(2, 7)}`;

type Listener = (p: unknown) => void;

export function createMockBackend(): Backend {
  const listeners = new Map<string, Set<Listener>>();
  const emit = <E extends EventName>(e: E, p: EventMap[E]) => listeners.get(e)?.forEach((cb) => cb(p));

  const t0 = now();
  const services: Service[] = [
    { id: "hr", name: "HR · นโยบายบุคคล", code: "HR", color: "#1F4FD1", files: [], excluded: [], folders: ["D:\\Docs\\HR"], model: "godev1", effort: "medium", topK: 5, systemPrompt: "", createdAt: t0 - 9e8, updatedAt: t0 - 6e5 },
    { id: "pos", name: "POS · ระบบหน้าร้าน", code: "POS", color: "#B45309", files: [], excluded: [], folders: ["D:\\Docs\\POS"], model: "gothink1", effort: "high", topK: 8, systemPrompt: "", createdAt: t0 - 8e8, updatedAt: t0 - 7.2e6 },
    { id: "pr", name: "จัดซื้อและสัญญา", code: "PR", color: "#0F766E", files: [], excluded: [], folders: ["D:\\Docs\\Procurement"], model: "godev2", effort: "medium", topK: 5, systemPrompt: "", createdAt: t0 - 7e8, updatedAt: t0 - 3e7 },
    { id: "mt", name: "ประชุมทีม", code: "MT", color: "#7C3AED", files: [], excluded: [], folders: ["D:\\Docs\\Meetings"], model: "godev1", effort: "low", topK: 5, systemPrompt: "", createdAt: t0 - 6e8, updatedAt: t0 - 8.6e7 },
  ];

  const doc = (id: number, name: string, chunks: number, mb: number, extra: Partial<DocItem> = {}): DocItem => ({
    id,
    path: `D:\\Docs\\${name}`,
    name,
    kind: name.split(".").pop()!.toUpperCase(),
    status: "ready",
    chunkCount: chunks,
    bytes: Math.round(mb * 1024 * 1024),
    progress: 1,
    message: null,
    ...extra,
  });

  const docs: Record<string, DocItem[]> = {
    hr: [
      doc(1, "นโยบายการลา_2569.pdf", 42, 2.4),
      doc(2, "สวัสดิการพนักงาน_2569.pdf", 36, 2.1),
      doc(3, "คู่มือพนักงานใหม่.docx", 18, 1.6, { status: "indexing", progress: 0.64, chunkCount: 0 }),
    ],
    pos: [
      doc(11, "คู่มือ POS v5.pdf", 120, 8.2),
      doc(12, "ขั้นตอนปิดยอดรายวัน.docx", 38, 2.2),
      doc(13, "รหัสข้อผิดพลาด.md", 24, 0.2),
      doc(14, "FAQ หน้าร้าน.txt", 30, 0.1),
    ],
    pr: [
      doc(21, "ระเบียบจัดซื้อ_2568.pdf", 58, 3.1),
      doc(22, "สัญญาแม่แบบ_สแกน.pdf", 0, 1.1, { status: "skipped", message: "ข้าม · PDF สแกน ไม่มีข้อความ" }),
    ],
    mt: [1, 2, 3, 4, 5, 6].map((i) => doc(30 + i, `บันทึกประชุม_ต.ค.${i}.md`, 12, 0.5)),
  };

  const chats: Chat[] = [
    { id: "c1", serviceId: "hr", title: "นโยบายการลาพักร้อน", createdAt: t0 - 3.6e6, updatedAt: t0 - 3e5 },
    { id: "c2", serviceId: "hr", title: "สิทธิ์ค่ารักษาพยาบาล", createdAt: t0 - 8.6e7, updatedAt: t0 - 8.6e7 },
    { id: "c3", serviceId: "hr", title: "ขั้นตอนรับพนักงานใหม่", createdAt: t0 - 2e8, updatedAt: t0 - 2e8 },
    { id: "c4", serviceId: "pos", title: "เครื่องพิมพ์ใบเสร็จไม่ทำงาน", createdAt: t0 - 4e7, updatedAt: t0 - 4e7 },
  ];

  const hrSources: Source[] = [
    { n: 1, serviceId: "hr", serviceName: "HR · นโยบายบุคคล", docPath: "D:\\Docs\\HR\\นโยบายการลา_2569.pdf", fileName: "นโยบายการลา_2569.pdf", page: 4, ord: 12, score: 0.86, text: "ข้อ 7 การลาพักผ่อนประจำปี\nพนักงานที่ปฏิบัติงานติดต่อกันครบ 1 ปี มีสิทธิ์ลาพักผ่อนประจำปีได้ 10 วันทำการ การลาต้องยื่นผ่านระบบล่วงหน้าอย่างน้อย 3 วันทำการ และได้รับอนุมัติจากผู้บังคับบัญชาก่อนวันลา" },
    { n: 2, serviceId: "hr", serviceName: "HR · นโยบายบุคคล", docPath: "D:\\Docs\\HR\\นโยบายการลา_2569.pdf", fileName: "นโยบายการลา_2569.pdf", page: 5, ord: 13, score: 0.79, text: "ข้อ 7.2 เมื่อพนักงานมีอายุงานครบ 3 ปี ให้ได้รับสิทธิ์ลาพักผ่อนเพิ่มขึ้นปีละ 1 วันทำการ รวมแล้วไม่เกิน 15 วันทำการต่อปี" },
  ];

  const messages: Record<string, Message[]> = {
    c1: [
      { id: 1, chatId: "c1", role: "user", content: "พนักงานลาพักร้อนได้กี่วันต่อปี และต้องแจ้งล่วงหน้ากี่วัน", model: null, reasoningEffort: null, sources: [], elapsedMs: null, error: null, createdAt: t0 - 3.6e6 },
      {
        id: 2, chatId: "c1", role: "assistant",
        content: "ตามนโยบายการลา พนักงานที่ทำงานครบ 1 ปีมีสิทธิ์ลาพักร้อน **10 วันทำการต่อปี** [1] และเพิ่มขึ้นปีละ 1 วันเมื่ออายุงานครบ 3 ปี สูงสุด 15 วัน [2]\n\nการลาต้องยื่นผ่านระบบ **ล่วงหน้าอย่างน้อย 3 วันทำการ** และได้รับอนุมัติจากหัวหน้างานก่อนวันลา [1]",
        model: "godev1", reasoningEffort: "medium", sources: hrSources, elapsedMs: 2400, error: null, createdAt: t0 - 3.59e6,
      },
    ],
  };

  let settings: Settings = {
    theme: "system", userName: "navaritrod", setupDone: false, historyTurns: 6, autoDeleteDays: 0,
    topK: 5, threshold: 0.3, chunkSize: 1000, chunkOverlap: 150, embeddingModel: "multilingual-e5-small", lastServiceId: "hr",
    updateUrl: "https://docask.example.internal/latest.json", autoCheckUpdate: true, ocrEnabled: true, keepImages: true, rerank: true, hybrid: true, indexLoad: "normal",
  };
  try {
    const saved = localStorage.getItem("docask-mock-settings");
    if (saved) settings = { ...settings, ...JSON.parse(saved) };
  } catch { /* storage unavailable */ }
  // preview helper: ?setup=done skips the wizard
  if (new URLSearchParams(location.search).get("setup") === "done") settings.setupDone = true;

  let gateway: GatewayInfo = {
    groupName: "Gosoft Agent Gateway", displayName: "Gosoft Agent Gateway",
    url: DEFAULT_GATEWAY_URL, defaultUrl: DEFAULT_GATEWAY_URL, apiType: "Chat Completions",
    hasKey: true, keyLast4: "x7Kp", lastTestAt: t0 - 3e6, lastTestPassed: 5, lastTestTotal: 6,
  };
  let models: ModelConfig[] = defaultModels(gateway.url).map((m) => (m.id === "gothink3" ? { ...m, hidden: true } : m));
  let status: GatewayStatus = { state: "ok", message: null, checkedAt: now() };
  let embedding: EmbeddingStatus = { model: "multilingual-e5-small", ready: true, path: "%APPDATA%\\DocAsk\\models\\multilingual-e5-small", sizeBytes: 118e6 };
  const streams = new Map<string, { stopped: boolean }>();
  let updateAvailable = false;
  let reranker: RerankStatus & { model: "mmarco-mMiniLMv2-L12" } = { model: "mmarco-mMiniLMv2-L12", ready: false, path: "%APPDATA%\\DocAsk\\models\\mmarco-mMiniLMv2-L12", sizeBytes: null };

  // simulate the indexing file in HR advancing
  const indexTimer = setInterval(() => {
    const d = docs.hr.find((x) => x.status === "indexing");
    if (!d) return clearInterval(indexTimer);
    d.progress = Math.min(1, d.progress + 0.04);
    emit("index-progress", { serviceId: "hr", jobId: "mock", docPath: d.path, fileName: d.name, done: 2, total: 3, docProgress: d.progress, phase: "embedding", message: null });
    if (d.progress >= 1) {
      d.status = "ready";
      d.chunkCount = 18;
      emit("index-progress", { serviceId: "hr", jobId: "mock", docPath: null, fileName: null, done: 3, total: 3, docProgress: 1, phase: "done", message: null });
    }
  }, 1200);

  const stats = (s: Service): ServiceStats => {
    const list = docs[s.id] ?? [];
    return {
      serviceId: s.id,
      documents: list.length,
      chunks: list.reduce((a, d) => a + d.chunkCount, 0),
      bytes: list.reduce((a, d) => a + d.bytes, 0),
      indexBytes: list.reduce((a, d) => a + d.chunkCount, 0) * 6500,
      skipped: list.filter((d) => d.status === "skipped").length,
      indexing: list.filter((d) => d.status === "indexing" || d.status === "queued").length,
      lastIndexedAt: s.updatedAt,
    };
  };

  const answerFor = (q: string, svc: Service): { text: string; sources: Source[] } => {
    if (/sql|คิวรี่|ตาราง/i.test(q) && svc.id === "hr") {
      return {
        text: "ดูผู้อนุมัติการลาได้จากตาราง **PS_GP_ABS_SS_STA** [1]\n\n```sql\nSELECT * FROM SYSADM.PS_GP_ABS_SS_STA WHERE EMPLID = '{EMPLID}';\n```\n[1]\n\nแทน `{EMPLID}` ด้วยรหัสพนักงาน 7 หลัก",
        sources: hrSources,
      };
    }
    if (svc.id === "hr") return { text: messages.c1[1].content, sources: hrSources };
    const list = (docs[svc.id] ?? []).filter((d) => d.status === "ready");
    const sources: Source[] = list.slice(0, 2).map((d, i) => ({
      n: i + 1, serviceId: svc.id, serviceName: svc.name, docPath: d.path, fileName: d.name, page: d.kind === "PDF" ? 3 + i : null, ord: i, score: 0.8 - i * 0.07,
      text: `(ข้อความตัวอย่างจาก ${d.name}) เนื้อหาที่เกี่ยวข้องกับคำถาม "${q.slice(0, 30)}" อยู่ในส่วนนี้ของเอกสาร`,
    }));
    if (!sources.length) return { text: "ไม่พบข้อมูลที่เกี่ยวข้องในเอกสารของ service นี้", sources: [] };
    return {
      text: `จากเอกสารใน ${svc.name} คำตอบโดยสรุปคือ **นี่เป็นคำตอบสมมติใน mock mode** [1] ซึ่งอ้างอิงจากเอกสารตัวอย่าง และมีรายละเอียดเพิ่มเติมในเอกสารอีกฉบับ [2]`,
      sources,
    };
  };

  const persist = () => {
    try { localStorage.setItem("docask-mock-settings", JSON.stringify(settings)); } catch { /* ignore */ }
  };

  const backend: Backend = {
    mock: true,
    appInfo: async () => ({ version: "0.1.0", dataDir: "%APPDATA%\\DocAsk", mock: true }),
    getSettings: async () => (await wait(150), clone(settings)),
    saveSettings: async (s) => { settings = clone(s); persist(); return clone(settings); },
    getGateway: async () => clone(gateway),
    setGatewayUrl: async (url) => { gateway = { ...gateway, url: url.trim().replace(/\/+$/, "") }; return clone(gateway); },
    resetGatewayUrl: async () => { gateway = { ...gateway, url: gateway.defaultUrl }; return clone(gateway); },
    saveApiKey: async (key) => {
      await wait(300);
      gateway = { ...gateway, hasKey: true, keyLast4: key.trim().slice(-4) };
      return clone(gateway);
    },
    removeProvider: async () => {
      gateway = { ...gateway, hasKey: false, keyLast4: null, url: gateway.defaultUrl, lastTestAt: null, lastTestPassed: null, lastTestTotal: null };
      models = defaultModels(gateway.url);
      settings = { ...settings, setupDone: false };
      persist();
    },
    gatewayStatus: async () => clone(status),
    checkGateway: async () => {
      await wait(500);
      status = gateway.hasKey ? { state: "ok", message: null, checkedAt: now() } : { state: "error", message: errorText(401, ""), checkedAt: now() };
      emit("gateway-status", status);
      return clone(status);
    },
    listModels: async () => clone(models),
    saveModels: async (m) => { models = clone(m); return clone(models); },
    resetModels: async () => { models = defaultModels(gateway.url); return clone(models); },
    importVsCodeModels: async () => { await wait(300); models = defaultModels(gateway.url); return clone(models); },
    testModels: async (only) => {
      const results: TestResult[] = [];
      for (const m of models.filter((x) => x.enabled && (!only || only.includes(x.id)))) {
        const elapsed = 900 + Math.round(Math.random() * 3000);
        await wait(350);
        const r: TestResult = m.id === "gothink3"
          ? { model: m.id, ok: false, reply: null, error: errorText(403, ""), status: 403, elapsedMs: 420 }
          : { model: m.id, ok: true, reply: "GOSOFT LITELLM OK", error: null, status: 200, elapsedMs: elapsed };
        results.push(r);
        emit("gateway-test-result", r);
      }
      models = models.map((m) => {
        const r = results.find((x) => x.model === m.id);
        return r ? { ...m, hidden: !r.ok } : m;
      });
      gateway = { ...gateway, lastTestAt: now(), lastTestPassed: results.filter((r) => r.ok).length, lastTestTotal: results.length };
      return results;
    },
    listServices: async () => (await wait(200), clone(services)),
    serviceStats: async () => services.map(stats),
    createService: async (n: NewService) => {
      const s: Service = { ...n, files: [], excluded: [], id: uid("svc"), createdAt: now(), updatedAt: now() };
      services.push(s);
      docs[s.id] = [];
      emit("services-changed", null);
      return clone(s);
    },
    updateService: async (s) => {
      const i = services.findIndex((x) => x.id === s.id);
      if (i >= 0) services[i] = { ...s, updatedAt: now() };
      emit("services-changed", null);
      return clone(services[i]);
    },
    deleteService: async (id, deleteChats) => {
      const i = services.findIndex((x) => x.id === id);
      if (i >= 0) services.splice(i, 1);
      delete docs[id];
      if (deleteChats) for (let j = chats.length - 1; j >= 0; j--) if (chats[j].serviceId === id) chats.splice(j, 1);
      emit("services-changed", null);
    },
    listDocuments: async (sid) => (await wait(150), clone(docs[sid] ?? [])),
    addDocuments: async (sid, paths) => {
      const list = (docs[sid] ??= []);
      for (const p of paths) {
        const name = p.split(/[\\/]/).pop() || p;
        list.push(doc(++seq, name, 20, 0.8, { path: p, status: "queued", chunkCount: 0, progress: 0 }));
      }
      emit("services-changed", null);
    },
    removeDocument: async (sid, id) => {
      const d = (docs[sid] ?? []).find((x) => x.id === id);
      const svc = services.find((x) => x.id === sid);
      if (d && svc) {
        if (svc.files.includes(d.path)) svc.files = svc.files.filter((f) => f !== d.path);
        else svc.excluded = [...svc.excluded, d.path];
      }
      docs[sid] = (docs[sid] ?? []).filter((x) => x.id !== id);
      emit("services-changed", null);
    },
    restoreDocument: async (sid, path) => {
      const svc = services.find((x) => x.id === sid)!;
      svc.excluded = svc.excluded.filter((f) => f !== path);
      const name = path.split(/[\\/]/).pop() || path;
      (docs[sid] ??= []).push(doc(++seq, name, 20, 0.8, { path, status: "queued", chunkCount: 0, progress: 0 }));
      emit("services-changed", null);
      return clone(svc);
    },
    // pictures: the first cited document of each answer "has" a screenshot and a diagram
    sourceImages: async (src) => (await wait(120), src.n === 1 ? [{ id: 1, w: 640, h: 400 }, { id: 2, w: 480, h: 480 }] : []),
    imageData: async (_sid, id) => mockPicture(id),
    reindexService: async (sid) => { emit("index-progress", { serviceId: sid, jobId: "mock", docPath: null, fileName: null, done: 0, total: 0, docProgress: 1, phase: "done", message: null }); },
    cancelIndex: async () => {},
    exportService: async (_sid, dest) => `${dest}\\docask-export.zip`,
    listChats: async (sid, query) => {
      const q = query.trim().toLowerCase();
      return clone(
        chats
          .filter((c) => (sid === null || c.serviceId === sid) && (!q || c.title.toLowerCase().includes(q)))
          .sort((a, b) => b.updatedAt - a.updatedAt),
      );
    },
    getChat: async (cid) => {
      const c = chats.find((x) => x.id === cid);
      if (!c) throw { code: "not_found", message: "ไม่พบแชตนี้" };
      return clone(c);
    },
    getMessages: async (cid) => (await wait(120), clone(messages[cid] ?? [])),
    renameChat: async (cid, title) => {
      const c = chats.find((x) => x.id === cid)!;
      c.title = title;
      return clone(c);
    },
    deleteChat: async (cid) => {
      const i = chats.findIndex((x) => x.id === cid);
      if (i >= 0) chats.splice(i, 1);
      delete messages[cid];
    },
    deleteAllChats: async () => { chats.splice(0); for (const k of Object.keys(messages)) delete messages[k]; },
    ask: async (req) => {
      if (status.state !== "ok") throw { code: "gateway_unreachable", message: errorText(0, "") };
      const svc = services.find((s) => s.id === req.serviceId)!;
      let chatId = req.chatId;
      if (!chatId) {
        chatId = uid("chat");
        chats.push({ id: chatId, serviceId: svc.id, title: req.question.trim().slice(0, 40), createdAt: now(), updatedAt: now() });
        messages[chatId] = [];
      }
      const userMessage: Message = { id: ++seq, chatId, role: "user", content: req.question, model: null, reasoningEffort: null, sources: [], elapsedMs: null, error: null, createdAt: now() };
      messages[chatId].push(userMessage);
      const requestId = req.requestId ?? uid("req");
      const st = { stopped: false };
      streams.set(requestId, st);
      const cid = chatId;
      (async () => {
        const started = now();
        const { text, sources } = answerFor(req.question, svc);
        const base = req.allServices ? sources.map((s) => ({ ...s })) : sources;
        const ranked = settings.rerank && reranker.ready ? base.map((s, i) => ({ ...s, rerankScore: 0.97 - i * 0.21 })) : base;
        const srcs = settings.hybrid ? ranked.map((s, i) => (i === 0 ? { ...s, keyword: true } : s)) : ranked;
        await wait(400);
        emit("chat-sources", { requestId, chatId: cid, sources: srcs });
        let out = "";
        const parts = text.match(/.{1,6}/gsu) ?? [];
        for (const p of parts) {
          if (st.stopped) break;
          await wait(35);
          out += p;
          emit("chat-delta", { requestId, chatId: cid, delta: p });
        }
        const msg: Message = { id: ++seq, chatId: cid, role: "assistant", content: out, model: req.model, reasoningEffort: req.effort, sources: srcs, elapsedMs: now() - started, error: st.stopped ? "หยุดโดยผู้ใช้" : null, createdAt: now() };
        messages[cid].push(msg);
        const c = chats.find((x) => x.id === cid);
        if (c) c.updatedAt = now();
        streams.delete(requestId);
        status = { state: "ok", message: null, checkedAt: now() };
        emit("gateway-status", status);
        emit("chat-done", { requestId, chatId: cid, message: msg });
      })();
      return { requestId, chatId, userMessage };
    },
    stop: async (rid) => { const s = streams.get(rid); if (s) s.stopped = true; },
    openSource: async () => {},
    embeddingStatus: async () => clone(embedding),
    downloadEmbedding: async (model) => {
      embedding = { ...embedding, model, ready: false };
      const total = 118_000_000;
      for (let d = 0; d <= total; d += total / 20) {
        await wait(120);
        emit("embed-download", { model, file: "onnx/model_quantized.onnx", downloaded: d, total, done: false, error: null });
      }
      embedding = { model, ready: true, path: `%APPDATA%\\DocAsk\\models\\${model}`, sizeBytes: total };
      emit("embed-download", { model, file: "", downloaded: total, total, done: true, error: null });
    },
    useEmbeddingFolder: async (model, folder) => { embedding = { model, ready: true, path: folder, sizeBytes: null }; return clone(embedding); },
    // like this company's Gateway today: no embedding model for the key
    gatewayEmbeddingModels: async () => { await wait(400); return []; },
    testGatewayEmbedding: async (model) => {
      await wait(500);
      if (!model.trim()) throw { code: "invalid", message: "ใส่ชื่อโมเดลก่อน" };
      return { dims: model.includes("3-small") ? 1536 : 1024, ms: 420, similar: 0.82, unrelated: 0.31 };
    },
    useGatewayEmbedding: async (model) => {
      await wait(500);
      settings = { ...settings, embeddingModel: `gateway:${model}` };
      embedding = { model: `gateway:${model}`, ready: true, path: null, sizeBytes: null };
      return clone(embedding);
    },
    reindexAll: async () => {},
    wipeAllData: async () => { try { localStorage.removeItem("docask-mock-settings"); } catch { /* ignore */ } location.reload(); },
    ocrStatus: async () => ({ available: true, enabled: settings.ocrEnabled }),
    rerankStatus: async () => ({ ...reranker }),
    downloadReranker: async () => {
      const total = 118.6e6;
      let got = 0;
      const t = setInterval(() => {
        got = Math.min(total, got + total / 8);
        const done = got >= total;
        if (done) { clearInterval(t); reranker = { ...reranker, ready: true, sizeBytes: total }; }
        emit("embed-download", { model: reranker.model, file: done ? "" : "onnx/model_quantized.onnx", downloaded: got, total, done, error: null });
      }, 250);
    },
    useRerankerFolder: async (folder) => (reranker = { ...reranker, ready: true, path: folder, sizeBytes: 118.6e6 }),
    checkUpdate: async () => {
      await wait(500);
      if (!settings.updateUrl) throw { code: "update_no_url", message: "ยังไม่ได้ตั้ง URL อัปเดต — ไปที่ ตั้งค่า › เกี่ยวกับ" };
      return updateAvailable
        ? { available: true, currentVersion: "0.1.0", version: "0.2.0", notes: "- ค้นเอกสารเร็วขึ้น\n- แก้การอ่าน PDF ภาษาไทย", date: "2026-11-20" }
        : { available: false, currentVersion: "0.1.0", version: null, notes: null, date: null };
    },
    installUpdate: async () => {
      const total = 9_500_000;
      for (let d = 0; d <= total; d += total / 15) {
        await wait(120);
        emit("update-progress", { downloaded: d, total, done: false });
      }
      emit("update-progress", { downloaded: total, total, done: true });
      await wait(600);
      location.reload();
    },
    pickFolder: async () => "D:\\Docs\\ตัวอย่าง",
    pickFiles: async () => ["D:\\Docs\\เอกสารใหม่.pdf"],
    pickJson: async () => "C:\\Users\\me\\AppData\\Roaming\\Code\\User\\chatLanguageModels.json",
    on: async (event, cb) => {
      let set = listeners.get(event);
      if (!set) listeners.set(event, (set = new Set()));
      set.add(cb as Listener);
      return () => set!.delete(cb as Listener);
    },
  };

  // dev helper: toggle simulated VPN loss from the console
  (window as unknown as { docaskMock: object }).docaskMock = {
    update(on: boolean) { updateAvailable = on; },
    vpn(on: boolean) {
      status = on ? { state: "ok", message: null, checkedAt: now() } : { state: "unreachable", message: errorText(0, ""), checkedAt: now() };
      emit("gateway-status", status);
    },
  };

  return backend;
}

/** SVG stand-ins for pictures kept from documents (mock only). */
function mockPicture(id: number): string {
  const svg = id === 1
    ? `<svg xmlns="http://www.w3.org/2000/svg" width="640" height="400"><rect width="640" height="400" fill="#eef2ff"/><rect x="0" y="0" width="640" height="44" fill="#1F4FD1"/><text x="20" y="29" font-size="18" fill="#fff" font-family="sans-serif">ระบบลางาน · ยื่นใบลา</text><text x="40" y="100" font-size="16" fill="#333" font-family="sans-serif">ประเภทการลา: ลาพักร้อน</text><rect x="40" y="120" width="300" height="36" rx="6" fill="#fff" stroke="#99a"/><text x="40" y="200" font-size="16" fill="#333" font-family="sans-serif">วันที่เริ่ม – วันที่สิ้นสุด</text><rect x="40" y="220" width="300" height="36" rx="6" fill="#fff" stroke="#99a"/><rect x="40" y="300" width="140" height="44" rx="8" fill="#1F4FD1"/><text x="70" y="328" font-size="16" fill="#fff" font-family="sans-serif">ส่งคำขอ</text></svg>`
    : `<svg xmlns="http://www.w3.org/2000/svg" width="480" height="480"><rect width="480" height="480" fill="#f8fafc"/><g font-family="sans-serif" font-size="16" fill="#111"><rect x="140" y="30" width="200" height="56" rx="10" fill="#dbeafe"/><text x="190" y="64">พนักงานยื่นใบลา</text><rect x="140" y="170" width="200" height="56" rx="10" fill="#fef3c7"/><text x="170" y="204">หัวหน้าอนุมัติ</text><rect x="140" y="310" width="200" height="56" rx="10" fill="#dcfce7"/><text x="185" y="344">HR บันทึกวันลา</text></g><path d="M240 86v84M240 226v84" stroke="#555" stroke-width="3"/></svg>`;
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
}
