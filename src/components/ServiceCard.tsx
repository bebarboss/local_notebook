import type { Service, ServiceStats } from "../lib/types";
import { formatBytes, timeAgo } from "../state";
import { CodeBadge } from "./ui";

export function ServiceCard({
  service, stats, active, onSettings, onAddDocs, onOpen,
}: {
  service: Service; stats: ServiceStats | undefined; active: boolean; onSettings: () => void; onAddDocs: () => void; onOpen: () => void;
}) {
  const status = active
    ? { text: "กำลังใช้งาน", warn: false }
    : stats?.indexing
      ? { text: `กำลังทำ index ${stats.indexing} ไฟล์`, warn: false }
      : stats?.skipped
        ? { text: `ข้าม ${stats.skipped} ไฟล์ (PDF สแกน)`, warn: true }
        : { text: `อัปเดต ${timeAgo(stats?.lastIndexedAt ?? service.updatedAt)}`, warn: false };
  return (
    <article className={`svc-card${active ? " active" : ""}`} aria-label={service.name}>
      <button onClick={onOpen} style={{ all: "unset", cursor: "pointer", display: "flex", alignItems: "center", gap: 10 }} aria-label={`ใช้ ${service.name} ในแชต`}>
        <CodeBadge code={service.code} color={service.color} />
        <div style={{ display: "flex", flexDirection: "column", minWidth: 0 }}>
          <span style={{ fontWeight: 700 }} className="ellipsis">{service.name}</span>
          <span style={{ fontSize: 12, color: status.warn ? "var(--warn-text)" : "var(--muted)" }}>{status.text}</span>
        </div>
      </button>
      <div className="svc-stats">
        <div><span className="muted">เอกสาร</span><b>{stats?.documents ?? 0}</b></div>
        <div><span className="muted">ท่อน</span><b>{stats?.chunks ?? 0}</b></div>
        <div><span className="muted">ขนาด</span><b>{formatBytes(stats?.bytes ?? 0)}</b></div>
      </div>
      <div className="svc-meta">
        โมเดล <span className="mono">{service.model}</span> · {service.effort} · top-k {service.topK}
        <br />
        โฟลเดอร์: {service.folders.length ? service.folders.join(", ") : "— (เพิ่มไฟล์ทีละไฟล์)"}
        {service.folders.length > 0 && " (ติดตามไฟล์ใหม่อัตโนมัติ)"}
      </div>
      <div style={{ display: "flex", gap: 8 }}>
        <button className="btn" style={{ flex: 1, minHeight: 38 }} onClick={onSettings}>ตั้งค่า</button>
        <button className="btn" style={{ flex: 1, minHeight: 38 }} onClick={onAddDocs}>+ เอกสาร</button>
      </div>
    </article>
  );
}
