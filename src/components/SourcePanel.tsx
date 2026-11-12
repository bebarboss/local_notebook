import { ExternalLink } from "lucide-react";
import { api } from "../lib/api";
import type { Source } from "../lib/types";
import { th } from "../i18n/th";
import { useApp } from "../state";
import { Modal } from "./ui";
import { SourceImageGrid } from "./SourceImages";

export function SourcePanel({ source, onClose }: { source: Source; onClose: () => void }) {
  const { toast } = useApp();
  return (
    <Modal title={source.knowledge ? `บันทึกความรู้ [${source.n}]` : `แหล่งอ้างอิง [${source.n}]`} onClose={onClose} width={620}>
      {source.knowledge && (
        <p className="muted" style={{ fontSize: 13, lineHeight: 1.7, margin: 0 }}>
          บันทึกจากแชตก่อนหน้า ไม่ใช่เอกสารต้นฉบับ — แก้หรือลบได้ที่ไฟล์นี้ (ลบไฟล์ = เลิกใช้บันทึกนี้)
        </p>
      )}
      <div className="kv">
        <div><span className="k">{source.knowledge ? "บันทึก" : "ไฟล์"}</span><span style={{ wordBreak: "break-all" }}>{source.fileName}</span></div>
        <div><span className="k">Service</span><span>{source.serviceName}</span></div>
        <div><span className="k">ตำแหน่ง · ท่อนที่</span><span>{source.page != null ? th.pageOf(source.fileName, source.page) : "—"} · {source.ord + 1}</span></div>
        <div>
          <span className="k">ความเกี่ยวข้อง</span>
          <span className="mono">
            {source.rerankScore != null ? <>{source.rerankScore.toFixed(3)} <span className="muted">(reranker · vector {source.score.toFixed(3)})</span></> : source.score.toFixed(3)}
            {source.keyword && <span className="muted"> · ตรงคำค้น</span>}
          </span>
        </div>
      </div>
      <div
        style={{ background: "var(--sidebar)", borderRadius: 12, padding: "14px 16px", lineHeight: 1.85, fontSize: 14, whiteSpace: "pre-wrap", maxHeight: 340, overflowY: "auto" }}
        tabIndex={0}
        aria-label="ข้อความท่อนต้นฉบับ"
      >
        {source.text}
      </div>
      <SourceImageGrid source={source} />
      <div className="modal-actions">
        <button
          className="btn btn-lg"
          onClick={async () => {
            try { await api.openSource(source.docPath); } catch (e) { toast((e as { message: string }).message); }
          }}
        >
          <ExternalLink size={16} strokeWidth={1.8} aria-hidden="true" />{th.openSource}
        </button>
        <button className="btn btn-lg btn-primary" onClick={onClose}>{th.close}</button>
      </div>
    </Modal>
  );
}
