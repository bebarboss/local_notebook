import { useCallback, useEffect, useState } from "react";
import { FileText, X, RefreshCw, Trash2, Undo2 } from "lucide-react";
import { api } from "../lib/api";
import type { DocItem, IndexProgress, Service } from "../lib/types";
import { formatBytes, useApp } from "../state";
import { ProgressBar } from "./ProgressBar";
import { ConfirmDialog, ErrorState, SkeletonList } from "./ui";

const baseName = (p: string) => p.split(/[\\/]/).pop() || p;

export function DocPanel({ service, onClose }: { service: Service; onClose: () => void }) {
  const { stats, toast, refreshServices } = useApp();
  const [docs, setDocs] = useState<DocItem[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [job, setJob] = useState<IndexProgress | null>(null);
  const [removing, setRemoving] = useState<DocItem | null>(null);

  const load = useCallback(async () => {
    setError(null);
    try { setDocs(await api.listDocuments(service.id)); } catch (e) { setError((e as { message: string }).message); }
  }, [service.id]);

  useEffect(() => { setDocs(null); load(); }, [load]);

  useEffect(() => {
    const off = api.on("index-progress", (p) => {
      if (p.serviceId !== service.id) return;
      setJob(p.phase === "done" || p.phase === "cancelled" || p.phase === "error" ? null : p);
      if (p.phase === "error" && p.message) toast(p.message);
      setDocs((cur) => {
        if (!cur || !p.docPath) return cur;
        return cur.map((d) => (d.path === p.docPath ? { ...d, status: "indexing", progress: p.docProgress } : d));
      });
      if (p.phase === "done" || p.phase === "cancelled" || (p.docProgress >= 1 && p.docPath)) load();
    });
    return () => { off.then((f) => f()); };
  }, [service.id, load, toast]);

  const st = stats[service.id];
  const addDocs = async () => {
    const files = await api.pickFiles();
    if (!files.length) return;
    try {
      await api.addDocuments(service.id, files);
      toast(`เพิ่ม ${files.length} ไฟล์ กำลังทำ index`);
      await Promise.all([load(), refreshServices()]);
    } catch (e) { toast((e as { message: string }).message); }
  };

  const inFolder = (path: string) =>
    service.folders.some((f) => path.toLowerCase().startsWith(f.toLowerCase().replace(/[\\/]?$/, "\\")));

  const remove = async (d: DocItem) => {
    try {
      await api.removeDocument(service.id, d.id);
      setRemoving(null);
      toast(`เอา ${d.name} ออกจาก ${service.code} แล้ว`);
      await Promise.all([load(), refreshServices()]);
    } catch (e) { toast((e as { message: string }).message); }
  };

  const restore = async (path: string) => {
    try {
      await api.restoreDocument(service.id, path);
      toast(`นำ ${baseName(path)} กลับมาแล้ว กำลังทำ index`);
      await Promise.all([load(), refreshServices()]);
    } catch (e) { toast((e as { message: string }).message); }
  };

  return (
    <aside className="right-panel" aria-label={`เอกสารใน ${service.name}`}>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8 }}>
        <h2 style={{ fontSize: 15, fontWeight: 700 }} className="ellipsis">เอกสารใน {service.code}</h2>
        <div style={{ display: "flex", gap: 4 }}>
          <button className="btn" onClick={addDocs}>+ เพิ่ม</button>
          <button className="icon-btn" aria-label="ปิดแผงเอกสาร" onClick={onClose}><X size={16} strokeWidth={1.8} /></button>
        </div>
      </div>

      <div style={{ display: "flex", flexDirection: "column", gap: 2, fontSize: 12, color: "var(--muted)" }}>
        <span>{st?.documents ?? 0} เอกสาร · {st?.chunks ?? 0} ท่อน</span>
        <span>ไฟล์ต้นฉบับ {formatBytes(st?.bytes ?? 0)} · index ในเครื่อง {formatBytes(st?.indexBytes ?? 0)}</span>
      </div>

      {job && (
        <div className="info-box" role="status">
          <span><strong>กำลังทำ index</strong> {job.done}/{job.total} ไฟล์</span>
          {job.fileName && <span className="ellipsis">{job.fileName}</span>}
          <button className="link-btn" style={{ alignSelf: "flex-start" }} onClick={() => api.cancelIndex(service.id)}>ยกเลิก</button>
        </div>
      )}

      {error && <ErrorState message={error} onRetry={load} />}
      {!docs && !error && <SkeletonList rows={3} />}
      {docs && docs.length === 0 && (
        <div className="empty" style={{ padding: "20px 8px" }}>
          <FileText size={28} strokeWidth={1.8} aria-hidden="true" />
          <span>ยังไม่มีเอกสารใน service นี้</span>
          <button className="btn btn-primary" onClick={addDocs}>+ เพิ่มเอกสาร</button>
        </div>
      )}
      {docs && docs.length > 0 && (
        <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          {docs.map((d) => <DocCard key={d.id} d={d} onRemove={() => setRemoving(d)} />)}
        </div>
      )}

      {service.excluded.length > 0 && (
        <details className="info-box">
          <summary style={{ cursor: "pointer" }}><strong>ไฟล์ที่เอาออก ({service.excluded.length})</strong></summary>
          <span style={{ fontSize: 12, color: "var(--muted)" }}>อยู่ในโฟลเดอร์ที่ติดตาม แต่จะไม่ถูก index</span>
          {service.excluded.map((p) => (
            <div key={p} style={{ display: "flex", alignItems: "center", gap: 6 }} title={p}>
              <span className="ellipsis" style={{ flex: 1, minWidth: 0 }}>{baseName(p)}</span>
              <button className="link-btn" style={{ display: "inline-flex", gap: 4, alignItems: "center" }} onClick={() => restore(p)}>
                <Undo2 size={12} strokeWidth={1.8} aria-hidden="true" /> นำกลับ
              </button>
            </div>
          ))}
        </details>
      )}

      {removing && (
        <ConfirmDialog
          title="เอาเอกสารออกจาก service"
          body={
            <>
              เอา <strong style={{ wordBreak: "break-all" }}>{removing.name}</strong> ออกจาก {service.name}?
              ท่อนข้อความและรูปของไฟล์นี้จะถูกลบออกจาก index — <strong>ไฟล์ต้นฉบับในเครื่องไม่ถูกลบ</strong>
              {inFolder(removing.path) && <><br />ไฟล์นี้อยู่ในโฟลเดอร์ที่ติดตาม จะไม่ถูก index อีกจนกว่าจะกด "นำกลับ"</>}
            </>
          }
          confirmLabel="เอาออก"
          danger
          onConfirm={() => remove(removing)}
          onCancel={() => setRemoving(null)}
        />
      )}

      <div className="info-box">
        <strong>ค่าของ service นี้</strong>
        <span>โมเดลเริ่มต้น: <span className="mono">{service.model}</span> · {service.effort}</span>
        <span>top-k: {service.topK} · ค้นข้าม service: ปิด (เปิดได้ที่ช่องพิมพ์)</span>
        <button className="link-btn" style={{ alignSelf: "flex-start", display: "inline-flex", gap: 4, alignItems: "center", marginTop: 4 }} onClick={() => api.reindexService(service.id, false).then(() => toast("เริ่มตรวจไฟล์ใหม่/แก้ไข"))}>
          <RefreshCw size={12} strokeWidth={1.8} aria-hidden="true" /> ตรวจไฟล์ใหม่ตอนนี้
        </button>
      </div>
      <div className="info-box" style={{ marginTop: "auto" }}>เอกสารและ vector ของแต่ละ service เก็บแยกกันในเครื่องนี้</div>
    </aside>
  );
}

function DocCard({ d, onRemove }: { d: DocItem; onRemove: () => void }) {
  const head = (
    <div className="row">
      <span className="name">{d.name}</span>
      <button className="icon-btn doc-del" aria-label={`เอา ${d.name} ออก`} title="เอาออกจาก service" onClick={onRemove}>
        <Trash2 size={14} strokeWidth={1.8} />
      </button>
    </div>
  );
  if (d.status === "skipped") {
    return (
      <div className="doc-card warn" title={d.path}>
        {head}
        <span className="sub">{d.message ?? "ข้าม · PDF สแกน ไม่มีข้อความ"}</span>
      </div>
    );
  }
  if (d.status === "error") {
    return (
      <div className="doc-card err" title={d.path}>
        {head}
        <span className="sub">{d.message ?? "แปลงไฟล์ไม่สำเร็จ"}</span>
      </div>
    );
  }
  if (d.status === "indexing" || d.status === "queued") {
    return (
      <div className="doc-card" style={{ gap: 6 }} title={d.path}>
        {head}
        <ProgressBar thin value={d.progress} label={`ความคืบหน้า ${d.name}`} />
        <span className="sub">{d.status === "queued" ? "รอคิวทำ index" : `กำลังทำ index ${Math.round(d.progress * 100)}%`}</span>
      </div>
    );
  }
  return (
    <div className="doc-card" title={d.path}>
      {head}
      <span className="sub">{d.kind} · {d.chunkCount} ท่อน</span>
    </div>
  );
}
