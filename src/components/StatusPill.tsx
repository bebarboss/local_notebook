import type { GatewayStatus } from "../lib/types";
import { th } from "../i18n/th";

export function StatusPill({ status, okText = th.gatewayOk }: { status: GatewayStatus; okText?: string }) {
  const cls = status.state === "ok" ? "ok" : status.state === "unknown" ? "unknown" : "err";
  const text =
    status.state === "ok" ? okText :
    status.state === "unknown" ? th.gatewayChecking :
    status.state === "unreachable" ? th.gatewayDown : th.gatewayError;
  return (
    <span className={`status ${cls}`} title={status.message ?? undefined}>
      <span className="dot" aria-hidden="true" />
      {text}
    </span>
  );
}
