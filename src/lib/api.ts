// Thin wrapper over Tauri invoke/listen. Falls back to an in-memory mock when the
// page is not running inside Tauri, or when built with DOCASK_MOCK=1.
// The frontend never sees the API key — only hasKey + last 4 chars.

import type {
  KnowledgeDraft,
  KnowledgeInfo,
  NewNote,
  AppError,
  AppInfo,
  AskRequest,
  AskStarted,
  Chat,
  ChatDelta,
  ChatDone,
  ChatSources,
  DocItem,
  DownloadProgress,
  EmbeddingModelId,
  EmbeddingTest,
  EmbedServerInfo,
  EmbedServerTest,
  ExportResult,
  ShareProgress,
  EmbeddingStatus,
  GatewayInfo,
  GatewayStatus,
  ImageInfo,
  IndexProgress,
  Message,
  ModelConfig,
  RerankStatus,
  Service,
  ServiceStats,
  Settings,
  Source,
  TestResult,
  UpdateInfo,
  UpdateProgress,
} from "./types";
import { createMockBackend } from "./mock";



export interface EventMap {
  "chat-delta": ChatDelta;
  "chat-sources": ChatSources;
  "chat-done": ChatDone;
  "chat-error": { requestId: string; chatId: string; error: AppError };
  "index-progress": IndexProgress;
  "gateway-test-result": TestResult;
  "embed-download": DownloadProgress;
  "gateway-status": GatewayStatus;
  "services-changed": null;
  "update-progress": UpdateProgress;
  "share-progress": ShareProgress;
}

export type EventName = keyof EventMap;
export type Unlisten = () => void;

export interface NewService {
  name: string;
  code: string;
  color: string;
  folders: string[];
  model: string;
  effort: Service["effort"];
  topK: number;
  systemPrompt: string;
}

export interface Backend {
  mock: boolean;
  appInfo(): Promise<AppInfo>;
  // settings
  getSettings(): Promise<Settings>;
  /** started by Windows at sign-in (Run key, this user) */
  getAutostart(): Promise<boolean>;
  setAutostart(enabled: boolean): Promise<boolean>;
  saveSettings(s: Settings): Promise<Settings>;
  // gateway + key
  getGateway(): Promise<GatewayInfo>;
  setGatewayUrl(url: string): Promise<GatewayInfo>;
  resetGatewayUrl(): Promise<GatewayInfo>;
  saveApiKey(key: string): Promise<GatewayInfo>;
  removeProvider(): Promise<void>;
  gatewayStatus(): Promise<GatewayStatus>;
  checkGateway(): Promise<GatewayStatus>;
  // models
  listModels(): Promise<ModelConfig[]>;
  saveModels(models: ModelConfig[]): Promise<ModelConfig[]>;
  resetModels(): Promise<ModelConfig[]>;
  importVsCodeModels(path: string): Promise<ModelConfig[]>;
  testModels(models?: string[]): Promise<TestResult[]>;
  // services
  listServices(): Promise<Service[]>;
  serviceStats(): Promise<ServiceStats[]>;
  createService(s: NewService): Promise<Service>;
  updateService(s: Service): Promise<Service>;
  deleteService(id: string, deleteChats: boolean): Promise<void>;
  listDocuments(serviceId: string): Promise<DocItem[]>;
  addDocuments(serviceId: string, paths: string[]): Promise<void>;
  removeDocument(serviceId: string, docId: number): Promise<void>;
  restoreDocument(serviceId: string, path: string): Promise<Service>;
  sourceImages(src: Source): Promise<ImageInfo[]>;
  imageData(serviceId: string, id: number): Promise<string>;
  reindexService(serviceId: string, full: boolean): Promise<void>;
  cancelIndex(serviceId: string): Promise<void>;
  /** one .zip with the index and the documents */
  exportServiceZip(serviceId: string, destDir: string): Promise<ExportResult>;
  importServiceZip(zipPath: string, destDir: string, reembed: boolean): Promise<Service>;
  /** the files of `from` now live in `to` — follow them without re-indexing */
  moveServiceFolder(serviceId: string, from: string, to: string): Promise<Service>;
  // chats
  listChats(serviceId: string | null, query: string): Promise<Chat[]>;
  getChat(chatId: string): Promise<Chat>;
  getMessages(chatId: string): Promise<Message[]>;
  renameChat(chatId: string, title: string): Promise<Chat>;
  deleteChat(chatId: string): Promise<void>;
  deleteAllChats(): Promise<void>;
  ask(req: AskRequest): Promise<AskStarted>;
  stop(requestId: string): Promise<void>;
  openSource(path: string): Promise<void>;
  // knowledge notes
  knowledgeInfo(serviceId: string): Promise<KnowledgeInfo>;
  /** model writes notes from the chat (up to message `upto`); nothing saved yet */
  draftKnowledge(chatId: string, upto: number | null): Promise<KnowledgeDraft[]>;
  saveKnowledge(serviceId: string, chatId: string | null, date: string, notes: NewNote[]): Promise<string[]>;
  openKnowledgeFolder(serviceId: string): Promise<void>;
  // embedding
  embeddingStatus(): Promise<EmbeddingStatus>;
  downloadEmbedding(model: EmbeddingModelId): Promise<void>;
  useEmbeddingFolder(model: EmbeddingModelId, folder: string): Promise<EmbeddingStatus>;
  gatewayEmbeddingModels(): Promise<string[]>;
  testGatewayEmbedding(model: string): Promise<EmbeddingTest>;
  useGatewayEmbedding(model: string): Promise<EmbeddingStatus>;
  embedServerInfo(): Promise<EmbedServerInfo>;
  /** token: null keeps the saved one */
  setEmbedServer(url: string, token: string | null): Promise<EmbedServerInfo>;
  testEmbedServer(): Promise<EmbedServerTest>;
  useEmbedServer(model: string): Promise<EmbeddingStatus>;
  forgetEmbedServer(): Promise<EmbedServerInfo>;
  reindexAll(): Promise<void>;
  wipeAllData(): Promise<void>;
  // updates
  ocrStatus(): Promise<{ available: boolean; enabled: boolean }>;
  rerankStatus(): Promise<RerankStatus>;
  downloadReranker(): Promise<void>;
  useRerankerFolder(folder: string): Promise<RerankStatus>;
  checkUpdate(): Promise<UpdateInfo>;
  installUpdate(): Promise<void>;
  // dialogs
  pickFolder(): Promise<string | null>;
  pickFiles(): Promise<string[]>;
  pickZip(): Promise<string | null>;
  pickJson(): Promise<string | null>;
  // events
  on<E extends EventName>(event: E, cb: (payload: EventMap[E]) => void): Promise<Unlisten>;
}

function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function toAppError(e: unknown): AppError {
  if (e && typeof e === "object" && "message" in e) {
    const o = e as { code?: string; message: string };
    return { code: o.code ?? "unknown", message: String(o.message) };
  }
  return { code: "unknown", message: String(e) };
}

function createTauriBackend(): Backend {
  // lazy imports keep the browser/mock bundle free of Tauri internals at startup
  const core = import("@tauri-apps/api/core");
  const ev = import("@tauri-apps/api/event");
  const dialog = import("@tauri-apps/plugin-dialog");

  const call = async <T>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
    const { invoke } = await core;
    try {
      return await invoke<T>(cmd, args);
    } catch (e) {
      throw toAppError(e);
    }
  };

  return {
    mock: false,
    appInfo: () => call("app_info"),
    getSettings: () => call("get_settings"),
    getAutostart: () => call("get_autostart"),
    setAutostart: (enabled) => call("set_autostart", { enabled }),
    saveSettings: (settings) => call("save_settings", { settings }),
    getGateway: () => call("get_gateway"),
    setGatewayUrl: (url) => call("set_gateway_url", { url }),
    resetGatewayUrl: () => call("reset_gateway_url"),
    saveApiKey: (key) => call("save_api_key", { key }),
    removeProvider: () => call("remove_provider"),
    gatewayStatus: () => call("gateway_status"),
    checkGateway: () => call("check_gateway"),
    listModels: () => call("list_models"),
    saveModels: (models) => call("save_models", { models }),
    resetModels: () => call("reset_models"),
    importVsCodeModels: (path) => call("import_vscode_models", { path }),
    testModels: (models) => call("test_models", { models: models ?? null }),
    listServices: () => call("list_services"),
    serviceStats: () => call("service_stats"),
    createService: (service) => call("create_service", { service }),
    updateService: (service) => call("update_service", { service }),
    deleteService: (id, deleteChats) => call("delete_service", { id, deleteChats }),
    listDocuments: (serviceId) => call("list_documents", { serviceId }),
    addDocuments: (serviceId, paths) => call("add_documents", { serviceId, paths }),
    removeDocument: (serviceId, docId) => call("remove_document", { serviceId, docId }),
    restoreDocument: (serviceId, path) => call("restore_document", { serviceId, path }),
    sourceImages: (s) => call("source_images", { serviceId: s.serviceId, docPath: s.docPath, page: s.page, ord: s.ord }),
    imageData: (serviceId, id) => call("image_data", { serviceId, id }),
    reindexService: (serviceId, full) => call("reindex_service", { serviceId, full }),
    cancelIndex: (serviceId) => call("cancel_index", { serviceId }),
    exportServiceZip: (serviceId, destDir) => call("export_service_zip", { serviceId, destDir }),
    importServiceZip: (zipPath, destDir, reembed) => call("import_service_zip", { zipPath, destDir, reembed }),
    moveServiceFolder: (serviceId, from, to) => call("move_service_folder", { serviceId, from, to }),
    listChats: (serviceId, query) => call("list_chats", { serviceId, query }),
    getChat: (chatId) => call("get_chat", { chatId }),
    getMessages: (chatId) => call("get_messages", { chatId }),
    renameChat: (chatId, title) => call("rename_chat", { chatId, title }),
    deleteChat: (chatId) => call("delete_chat", { chatId }),
    deleteAllChats: () => call("delete_all_chats"),
    ask: (req) => call("ask", { req }),
    stop: (requestId) => call("stop_generation", { requestId }),
    openSource: (path) => call("open_source_file", { path }),
    knowledgeInfo: (serviceId) => call("knowledge_info", { serviceId }),
    draftKnowledge: (chatId, upto) => call("draft_knowledge", { chatId, upto }),
    saveKnowledge: (serviceId, chatId, date, notes) => call("save_knowledge", { serviceId, chatId, date, notes }),
    openKnowledgeFolder: (serviceId) => call("open_knowledge_folder", { serviceId }),
    embeddingStatus: () => call("embedding_status"),
    downloadEmbedding: (model) => call("download_embedding", { model }),
    useEmbeddingFolder: (model, folder) => call("use_embedding_folder", { model, folder }),
    gatewayEmbeddingModels: () => call("gateway_embedding_models"),
    testGatewayEmbedding: (model) => call("test_gateway_embedding", { model }),
    useGatewayEmbedding: (model) => call("use_gateway_embedding", { model }),
    embedServerInfo: () => call("embed_server_info"),
    setEmbedServer: (url, token) => call("set_embed_server", { url, token }),
    testEmbedServer: () => call("test_embed_server"),
    useEmbedServer: (model) => call("use_embed_server", { model }),
    forgetEmbedServer: () => call("forget_embed_server"),
    reindexAll: () => call("reindex_all"),
    wipeAllData: () => call("wipe_all_data"),
    ocrStatus: () => call("ocr_status"),
    rerankStatus: () => call("rerank_status"),
    downloadReranker: () => call("download_reranker"),
    useRerankerFolder: (folder) => call("use_reranker_folder", { folder }),
    checkUpdate: () => call("check_update"),
    installUpdate: () => call("install_update"),
    pickFolder: async () => {
      const { open } = await dialog;
      const r = await open({ directory: true, multiple: false });
      return typeof r === "string" ? r : null;
    },
    pickFiles: async () => {
      const { open } = await dialog;
      const r = await open({
        multiple: true,
        filters: [{ name: "เอกสารและรูปภาพ", extensions: ["pdf", "docx", "pptx", "xlsx", "xlsm", "xls", "ods", "csv", "tsv", "txt", "md", "sql", "png", "jpg", "jpeg", "bmp", "webp", "tif", "tiff"] }],
      });
      if (!r) return [];
      return Array.isArray(r) ? r : [r];
    },
    pickZip: async () => {
      const { open } = await dialog;
      const r = await open({ multiple: false, filters: [{ name: "Service ที่ส่งออกจาก DocAsk", extensions: ["zip"] }] });
      return typeof r === "string" ? r : null;
    },
    pickJson: async () => {
      const { open } = await dialog;
      const r = await open({ multiple: false, filters: [{ name: "JSON", extensions: ["json"] }] });
      return typeof r === "string" ? r : null;
    },
    on: async (event, cb) => {
      const { listen } = await ev;
      return listen(event, (e) => cb(e.payload as never));
    },
  };
}

export const api: Backend = !inTauri() || __DOCASK_MOCK__ ? createMockBackend() : createTauriBackend();
