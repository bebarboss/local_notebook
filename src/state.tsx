import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api } from "./lib/api";
import type { AskRequest, GatewayStatus, ModelConfig, Service, ServiceStats, Settings } from "./lib/types";

export type Page =
  | { name: "chat"; chatId: string; initial?: AskRequest }
  | { name: "newchat" }
  | { name: "services" }
  | { name: "settings"; section?: SettingsSection }
  | { name: "setup" };

export type SettingsSection = "gateway" | "models" | "embedding" | "search" | "history" | "theme" | "trouble" | "about";

interface Ctx {
  ready: boolean;
  loadError: string | null;
  settings: Settings;
  updateSettings: (patch: Partial<Settings>) => Promise<void>;
  services: Service[];
  stats: Record<string, ServiceStats>;
  refreshServices: () => Promise<void>;
  models: ModelConfig[];
  refreshModels: () => Promise<void>;
  gateway: GatewayStatus;
  recheckGateway: () => Promise<void>;
  page: Page;
  go: (p: Page) => void;
  /** service shown in the sidebar (selected list) */
  activeServiceId: string | null;
  setActiveServiceId: (id: string | null) => void;
  chatsVersion: number;
  bumpChats: () => void;
  toast: (msg: string) => void;
  reload: () => Promise<void>;
}

const AppCtx = createContext<Ctx | null>(null);

export const DEFAULT_SETTINGS: Settings = {
  theme: "system",
  userName: "",
  setupDone: false,
  historyTurns: 6,
  autoDeleteDays: 0,
  topK: 5,
  threshold: 0.3,
  chunkSize: 1000,
  chunkOverlap: 150,
  embeddingModel: "multilingual-e5-small",
  lastServiceId: null,
  updateUrl: "",
  autoCheckUpdate: true,
  ocrEnabled: true,
  keepImages: true,
  rerank: true,
  hybrid: true,
};

export function AppProvider({ children }: { children: ReactNode }) {
  const [ready, setReady] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [settings, setSettings] = useState<Settings>(DEFAULT_SETTINGS);
  const [services, setServices] = useState<Service[]>([]);
  const [stats, setStats] = useState<Record<string, ServiceStats>>({});
  const [models, setModels] = useState<ModelConfig[]>([]);
  const [gateway, setGateway] = useState<GatewayStatus>({ state: "unknown", message: null, checkedAt: 0 });
  const [page, setPage] = useState<Page>({ name: "newchat" });
  const [activeServiceId, setActiveServiceId] = useState<string | null>(null);
  const [chatsVersion, setChatsVersion] = useState(0);
  const [toastMsg, setToastMsg] = useState<string | null>(null);
  const toastTimer = useRef<number>();

  const refreshServices = useCallback(async () => {
    const [svcs, st] = await Promise.all([api.listServices(), api.serviceStats()]);
    setServices(svcs);
    setStats(Object.fromEntries(st.map((s) => [s.serviceId, s])));
  }, []);

  const refreshModels = useCallback(async () => setModels(await api.listModels()), []);

  const recheckGateway = useCallback(async () => {
    try {
      setGateway(await api.checkGateway());
    } catch (e) {
      setGateway({ state: "error", message: String((e as { message?: string }).message ?? e), checkedAt: Date.now() });
    }
  }, []);

  const reload = useCallback(async () => {
    setLoadError(null);
    try {
      const s = await api.getSettings();
      setSettings(s);
      await Promise.all([refreshServices(), refreshModels()]);
      setActiveServiceId((cur) => cur ?? s.lastServiceId);
      setPage(s.setupDone ? initialPage() : { name: "setup" });
      setReady(true);
      if (s.setupDone) recheckGateway();
    } catch (e) {
      setLoadError(String((e as { message?: string }).message ?? e));
    }
  }, [refreshServices, refreshModels, recheckGateway]);

  useEffect(() => {
    reload();
  }, [reload]);

  // theme
  useEffect(() => {
    document.documentElement.dataset.theme = settings.theme;
  }, [settings.theme]);

  // live events
  useEffect(() => {
    const offs: Promise<() => void>[] = [
      api.on("gateway-status", (s) => setGateway(s)),
      api.on("services-changed", () => refreshServices()),
      api.on("index-progress", (p) => {
        if (p.phase === "done" || p.phase === "cancelled" || p.phase === "error") refreshServices();
      }),
    ];
    // periodic re-check so the VPN banner clears/appears on its own
    const t = window.setInterval(() => {
      if (settings.setupDone) api.gatewayStatus().then(setGateway).catch(() => {});
    }, 30_000);
    return () => {
      offs.forEach((p) => p.then((f) => f()));
      clearInterval(t);
    };
  }, [refreshServices, settings.setupDone]);

  const updateSettings = useCallback(
    async (patch: Partial<Settings>) => {
      const next = { ...settings, ...patch };
      setSettings(next);
      setSettings(await api.saveSettings(next));
    },
    [settings],
  );

  const toast = useCallback((msg: string) => {
    setToastMsg(msg);
    clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToastMsg(null), 2600);
  }, []);

  const value = useMemo<Ctx>(
    () => ({
      ready, loadError, settings, updateSettings, services, stats, refreshServices, models, refreshModels,
      gateway, recheckGateway, page, go: setPage, activeServiceId, setActiveServiceId,
      chatsVersion, bumpChats: () => setChatsVersion((v) => v + 1), toast, reload,
    }),
    [ready, loadError, settings, updateSettings, services, stats, refreshServices, models, refreshModels, gateway, recheckGateway, page, activeServiceId, chatsVersion, toast, reload],
  );

  return (
    <AppCtx.Provider value={value}>
      {children}
      {toastMsg && <div className="toast" role="status">{toastMsg}</div>}
    </AppCtx.Provider>
  );
}

export function useApp(): Ctx {
  const c = useContext(AppCtx);
  if (!c) throw new Error("useApp outside AppProvider");
  return c;
}

/** mock-mode preview helper: ?page=chat|services|settings|setup */
function initialPage(): Page {
  if (!api.mock) return { name: "newchat" };
  const p = new URLSearchParams(location.search).get("page");
  if (p === "chat") return { name: "chat", chatId: "c1" };
  if (p === "services" || p === "setup") return { name: p };
  if (p === "settings") return { name: "settings" };
  return { name: "newchat" };
}

export function initials(name: string): string {
  const n = name.trim();
  if (!n) return "?";
  const parts = n.split(/[\s._-]+/).filter(Boolean);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return n.slice(0, 2).toUpperCase();
}

export function formatBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(0)} KB`;
  if (b < 1024 ** 3) return `${(b / 1024 / 1024).toFixed(1)} MB`;
  return `${(b / 1024 ** 3).toFixed(2)} GB`;
}

export function timeAgo(ms: number | null): string {
  if (!ms) return "ยังไม่เคย";
  const d = (Date.now() - ms) / 1000;
  if (d < 60) return "เมื่อสักครู่";
  if (d < 3600) return `${Math.floor(d / 60)} นาทีที่แล้ว`;
  if (d < 86400) return `${Math.floor(d / 3600)} ชม. ที่แล้ว`;
  if (d < 172800) return "เมื่อวาน";
  return `${Math.floor(d / 86400)} วันที่แล้ว`;
}

export function visibleModels(models: ModelConfig[]): ModelConfig[] {
  return models.filter((m) => m.enabled && !m.hidden);
}
