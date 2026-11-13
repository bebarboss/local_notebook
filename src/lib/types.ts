// Shared types — must mirror the serde structs in src-tauri (camelCase).

export type Effort = "low" | "medium" | "high";
export type ThemeMode = "light" | "dark" | "system";
export type FontChoice = "looped" | "sarabun" | "plex" | "system";
export type TextSize = "s" | "m" | "l" | "xl";

export interface Service {
  id: string;
  name: string;
  code: string; // 2–3 letters
  color: string;
  folders: string[];
  /** individual files added with "+ เพิ่ม" outside the watched folders */
  files: string[];
  /** files inside the folders that were removed from this service (skipped by the scan) */
  excluded: string[];
  model: string;
  effort: Effort;
  topK: number;
  systemPrompt: string;
  /** where "บันทึกเป็นความรู้" writes notes ("" = default, chosen on the first save) */
  knowledgeDir: string;
  createdAt: number;
  updatedAt: number;
}

/** a note drafted from a chat, not saved yet */
export interface KnowledgeDraft {
  title: string;
  markdown: string;
}

export interface NewNote extends KnowledgeDraft {
  status: "fixed" | "unverified";
}

export interface KnowledgeInfo {
  dir: string;
  notes: number;
}

export interface ServiceStats {
  serviceId: string;
  documents: number;
  chunks: number;
  /** total size of the source files */
  bytes: number;
  /** size of the index on disk (text + vectors + pictures) */
  indexBytes: number;
  skipped: number;
  indexing: number;
  lastIndexedAt: number | null;
}

export type DocStatus = "ready" | "indexing" | "queued" | "skipped" | "error";

export interface DocItem {
  id: number;
  path: string;
  name: string;
  kind: string; // PDF / DOCX / TXT / MD
  status: DocStatus;
  chunkCount: number;
  bytes: number;
  progress: number; // 0..1 while indexing
  message: string | null; // reason for skip/error
}

export interface Source {
  n: number;
  /** a knowledge note saved from an earlier chat */
  knowledge?: boolean;
  serviceId: string;
  serviceName: string;
  docPath: string;
  fileName: string;
  page: number | null;
  ord: number;
  score: number;
  /** cross-encoder relevance 0–1 when reranking was used */
  rerankScore?: number | null;
  /** found by keyword (exact words) search */
  keyword?: boolean;
  text: string;
}

export interface RerankStatus {
  model: string;
  ready: boolean;
  path: string | null;
  sizeBytes: number | null;
}

/** A picture kept from a document (fetch the pixels with api.imageData). */
export interface ImageInfo {
  id: number;
  w: number;
  h: number;
}

export type Role = "user" | "assistant";

export interface Message {
  id: number;
  chatId: string;
  role: Role;
  content: string;
  model: string | null;
  reasoningEffort: Effort | null;
  sources: Source[];
  elapsedMs: number | null;
  error: string | null;
  createdAt: number;
  /** a follow-up question was searched with this standalone query */
  searchQuery?: string;
}

export interface Chat {
  id: string;
  serviceId: string;
  title: string;
  createdAt: number;
  updatedAt: number;
}

export interface ModelConfig {
  id: string;
  name: string;
  url: string;
  toolCalling: boolean;
  vision: boolean;
  thinking: boolean;
  contextWindow: number;
  maxOutputTokens: number;
  supportsReasoningEffort: Effort[];
  reasoningEffortFormat: string;
  enabled: boolean;
  /** hidden after failing the connection test */
  hidden: boolean;
}

export interface GatewayInfo {
  groupName: string;
  displayName: string;
  url: string;
  defaultUrl: string;
  apiType: string;
  hasKey: boolean;
  keyLast4: string | null;
  lastTestAt: number | null;
  lastTestPassed: number | null;
  lastTestTotal: number | null;
}

export type GatewayState = "unknown" | "ok" | "unreachable" | "error";

export interface GatewayStatus {
  state: GatewayState;
  message: string | null;
  checkedAt: number;
}

export interface TestResult {
  model: string;
  ok: boolean;
  reply: string | null;
  error: string | null;
  status: number | null;
  elapsedMs: number;
}

export interface Settings {
  theme: ThemeMode;
  userName: string;
  setupDone: boolean;
  historyTurns: number;
  autoDeleteDays: number; // 0 = never
  topK: number;
  threshold: number;
  chunkSize: number;
  chunkOverlap: number;
  /** a local model id, or "gateway:<model>" for an embedding model on the Gateway */
  embeddingModel: EmbeddingModelId | `gateway:${string}` | `server:${string}`;
  /** search the team database on the server; local indexes sync up */
  teamServer: boolean;
  lastServiceId: string | null;
  /** latest.json URL for in-app updates; empty = updates off */
  updateUrl: string;
  autoCheckUpdate: boolean;
  /** OCR pictures, scanned PDF pages and image files */
  ocrEnabled: boolean;
  /** keep pictures from documents and show them with answers */
  keepImages: boolean;
  /** re-order search hits with the reranker model (when downloaded) */
  rerank: boolean;
  /** fuse keyword (exact words) search with vector search */
  hybrid: boolean;
  indexLoad: "light" | "normal" | "fast";
  font: FontChoice;
  textSize: TextSize;
  startHidden: boolean;
}


export interface ExportResult {
  path: string;
  documents: number;
  bytes: number;
  /** documents in the index whose file no longer exists (left out of the zip) */
  missing: number;
}

export interface ShareProgress {
  phase: "export" | "import";
  done: number;
  total: number;
}

export interface EmbeddingTest {
  dims: number;
  ms: number;
  /** cosine of two Thai sentences with the same meaning, and of an unrelated pair */
  similar: number;
  unrelated: number;
}

/** DocAsk's own embedding server (server/embed) — the token itself never reaches the UI */
export interface EmbedServerInfo {
  url: string;
  hasToken: boolean;
  tokenLast4: string | null;
  /** the URL (and maybe the token) came with the program */
  builtin: boolean;
  /** the token is built in and fixed — only the URL can change */
  tokenLocked: boolean;
}

export interface EmbedServerTest extends EmbeddingTest {
  /** model id the server serves, e.g. "bge-m3" */
  model: string;
  device: string;
  /** the same model on this PC, used for questions while the server is down (null: none exists) */
  fallbackModel: EmbeddingModelId | null;
  fallbackReady: boolean;
  /** cosine of server vs local vector for one question (≈ 1: interchangeable) */
  fallbackMatch: number | null;
}

export interface TeamStoreInfo {
  model: string;
  dim: number;
  documents: number;
  chunks: number;
  chats: number;
}

export interface TeamStatus {
  enabled: boolean;
  info: TeamStoreInfo | null;
  /** why the server's database could not be reached */
  error: string | null;
  /** this PC's name, shown on the documents it uploads */
  machine: string;
}

/** a docask:// setup link waiting for "เชื่อมต่อ?" (never the token) */
export interface ConnectLinkInfo {
  host: string;
  url: string;
}

export interface ConnectResult {
  team: TeamStatus;
  teamError: string | null;
}

export interface TeamSync {
  serviceId: string;
  uploaded: number;
  deleted: number;
  unchanged: number;
  error: string | null;
}

export type EmbeddingModelId = "multilingual-e5-small" | "multilingual-e5-base" | "bge-m3";

export interface EmbeddingStatus {
  /** local model id, "gateway:<model>" or "server:<model>" */
  model: string;
  ready: boolean;
  path: string | null;
  sizeBytes: number | null;
}

export interface DownloadProgress {
  /** the reranker model name */
  model: string;
  file: string;
  downloaded: number;
  total: number | null;
  done: boolean;
  error: string | null;
}

export interface IndexProgress {
  serviceId: string;
  jobId: string;
  docPath: string | null;
  fileName: string | null;
  done: number;
  total: number;
  docProgress: number;
  phase: "scanning" | "extracting" | "embedding" | "done" | "cancelled" | "error";
  message: string | null;
}

export interface AskRequest {
  /** generated by the frontend so events can be matched before ask() returns */
  requestId?: string;
  chatId: string | null;
  serviceId: string;
  question: string;
  model: string;
  effort: Effort;
  allServices: boolean;
}

export interface AskStarted {
  requestId: string;
  chatId: string;
  userMessage: Message;
}

export interface ChatDelta {
  requestId: string;
  chatId: string;
  delta: string;
}

export interface ChatSources {
  requestId: string;
  chatId: string;
  sources: Source[];
  searchQuery?: string;
}

export interface ChatDone {
  requestId: string;
  chatId: string;
  message: Message;
}

export interface AppInfo {
  version: string;
  dataDir: string;
  mock: boolean;
}

export interface AppError {
  code: string;
  message: string;
}

export interface UpdateInfo {
  available: boolean;
  currentVersion: string;
  version: string | null;
  notes: string | null;
  date: string | null;
}

export interface UpdateProgress {
  downloaded: number;
  total: number | null;
  done: boolean;
}
