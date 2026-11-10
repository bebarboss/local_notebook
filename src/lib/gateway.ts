// Values from the "Gosoft Agent Gateway • VS Code User Guide". Mirrors src-tauri/src/config.rs.
import type { ModelConfig } from "./types";
import { th } from "../i18n/th";

export const DEFAULT_GATEWAY_URL = "https://agent-gateway.gosoft.co.th";
export const MODEL_IDS = ["godev1", "godev2", "godev3", "gothink1", "gothink2", "gothink3"] as const;

export function defaultModels(baseUrl: string = DEFAULT_GATEWAY_URL): ModelConfig[] {
  return MODEL_IDS.map((id) => ({
    id,
    name: id,
    url: `${baseUrl.replace(/\/+$/, "")}/v1/chat/completions`,
    toolCalling: true,
    vision: false,
    thinking: true,
    contextWindow: 1_000_000,
    maxOutputTokens: 64_000,
    supportsReasoningEffort: ["low", "medium", "high"],
    reasoningEffortFormat: "chat-completions",
    enabled: true,
    hidden: false,
  }));
}

/** Gateway error text per SPEC §3.3. status 0 = network / VPN. */
export function errorText(status: number, serverMessage: string): string {
  switch (status) {
    case 0: return th.err.unreachable;
    case 401: return th.err.e401;
    case 403: return th.err.e403;
    case 404: return th.err.e404;
    case 429: return th.err.e429;
    default: return `HTTP ${status}: ${redact(serverMessage)}`;
  }
}

/** Defense in depth — the backend already redacts. */
export function redact(s: string): string {
  return s.replace(/sk-[A-Za-z0-9_\-.]{2,}/g, "sk-***");
}
