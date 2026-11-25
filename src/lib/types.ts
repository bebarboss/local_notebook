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
  /** lives in the team database on the server (shared by the owner, or joined with an invite) */
  shared?: boolean;
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

/** A note like the one just saved (cosine of their first chunks). */
export interface SimilarNote {
  path: string;
  title: string;
  similarity: number;
  /** the note's document on the server (shared services) */
  documentId?: number;
}

/** "knowledge-duplicate": a saved note has notes of the same problem in its service. */
export interface KnowledgeDuplicate {
  serviceId: string;
  path: string;
  title: string;
  similar: SimilarNote[];
}

export interface NoteToHide {
  path: string;
  documentId?: number;
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
  /** a web result/page from the "ค้นเว็บ" research step (docPath is the URL) */
  web?: boolean;
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
  /** the agentic gather step's activity (empty/absent when it didn't run) */
  steps?: AgentStep[];
  /** files changed by a "เขียนโค้ด" turn (absent when it didn't run) — can be undone */
  changes?: Changeset;
}

/** one file changed by the coding agent */
export interface CodeChange {
  /** path relative to the project root, forward-slashed */
  path: string;
  kind: "create" | "modify";
  /** original content before this turn (null when the file was created) */
  before: string | null;
  after: string;
}

/** the files a coding turn changed, plus the project root they apply to (for undo) */
export interface Changeset {
  root: string;
  items: CodeChange[];
}

/** how many files an undo restored and deleted */
export interface UndoResult {
  restored: number;
  deleted: number;
}

export interface Chat {
  id: string;
  /** null for a general (floating) chat not tied to any service */
  serviceId: string | null;
  title: string;
  createdAt: number;
  updatedAt: number;
  /** "docs" = RAG over a service · "chat" = general assistant · "code" = coding agent on a folder */
  mode: "docs" | "chat" | "code";
  /** the project folder a "code" chat works in (absolute path); absent for other modes */
  projectDir?: string | null;
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

/** Usage/budget for the current virtual key (LiteLLM `/key/info`). All optional. */
export interface KeyUsage {
  keyAlias: string | null;
  spend: number | null;
  maxBudget: number | null;
  softBudget: number | null;
  budgetDuration: string | null;
  budgetResetAt: string | null;
  expires: string | null;
  tpmLimit: number | null;
  rpmLimit: number | null;
  maxParallelRequests: number | null;
  models: string[];
}

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
  /** look for a note of the same problem after saving one */
  knowledgeDupCheck: boolean;
  /** how alike (0.5–0.99) counts as the same problem */
  knowledgeDupThreshold: number;
  /** coding mode: extra command prefixes the agent may run (on top of project defaults) */
  codeCommands: string[];
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
  /** has the server token (may share services); else a member using an invite key */
  owner: boolean;
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
  /** this PC has the server token: may share services and make invite keys */
  owner: boolean;
  /** totals on the server (owner only) */
  info: TeamStoreInfo | null;
  /** why the server's database could not be reached */
  error: string | null;
  /** this PC's name, shown on the documents it uploads */
  machine: string;
  sharedServices: number;
}

/** a docask:// setup link waiting for "เชื่อมต่อ?" (never the token) */
export interface ConnectLinkInfo {
  /** "connect" = server setup link, "join" = invite to one shared service */
  kind: "connect" | "join";
  host: string;
  url: string;
  serviceName: string | null;
}

export interface ConnectResult {
  team: TeamStatus;
  /** the service joined with an invite */
  service: Service | null;
}

export interface TeamSync {
  serviceId: string;
  uploaded: number;
  deleted: number;
  unchanged: number;
  /** original files uploaded / too large (> 50 MB) to upload */
  files: number;
  tooLarge: number;
  /** not sent: the same content is already in the service (another PC / path) */
  duplicates: { file: string; sameAs: string; from: string }[];
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
  /** required for a docs chat; null for a general ("chat" mode) chat */
  serviceId: string | null;
  question: string;
  model: string;
  effort: Effort;
  /** "docs" (default), "chat", or "code"; only used when creating a new chat */
  mode?: "docs" | "chat" | "code";
  allServices: boolean;
  /** file paths attached to this one question (general chat only); read fresh, not indexed */
  attachments?: string[];
  /** current date/time from the user's clock (Thai-formatted); set automatically in api.ask */
  now?: string;
  /** general chat only: run a web-research step (DuckDuckGo) before answering */
  web?: boolean;
  /** general chat only: run the "เขียนโค้ด" agent on `projectDir` before answering */
  code?: boolean;
  /** the project folder the coding agent may read/edit (required when `code` is on) */
  projectDir?: string | null;
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

/** one line of the agentic gather step's activity ("กำลังทำอะไร") */
export interface AgentStep {
  /** "search" | "read" | "done" | "error" */
  kind: string;
  /** search terms, source number, or error code — never passage text */
  detail: string;
  /** new passages this step added */
  found: number;
}

export interface ChatStep {
  requestId: string;
  chatId: string;
  step: AgentStep;
}

/** the coding agent's changeset, emitted when a "เขียนโค้ด" turn finishes */
export interface ChatChanges {
  requestId: string;
  chatId: string;
  root: string;
  changes: CodeChange[];
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
