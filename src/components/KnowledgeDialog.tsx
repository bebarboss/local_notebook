// "บันทึกเป็นความรู้": the model drafts notes (one per problem) from a chat; the user checks,
// edits and picks which to keep. Saved notes go to the service's knowledge folder and are
// indexed, so the next question about the same problem finds them first.
import { useCallback, useEffect, useState } from "react";
import { BookMarked, FolderOpen } from "lucide-react";
import { api } from "../lib/api";
import type { KnowledgeDraft, NewNote } from "../lib/types";
import { th } from "../i18n/th";
import { useApp } from "../state";
import { ErrorState, Modal, Skeleton } from "./ui";

interface Card extends NewNote {
  keep: boolean;
}

/** local date (the user's time zone), "2026-11-12" */
function today(): string {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

export function KnowledgeDialog({ chatId, serviceId, upto, onClose }: { chatId: string; serviceId: string; upto: number | null; onClose: () => void }) {
  const { toast, services } = useApp();
  const service = services.find((s) => s.id === serviceId);
  const [cards, setCards] = useState<Card[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [dir, setDir] = useState<string | null>(null);

  const draft = useCallback(async () => {
    setError(null);
    setCards(null);
    try {
      const d: KnowledgeDraft[] = await api.draftKnowledge(chatId, upto);
      setCards(d.map((x) => ({ ...x, status: "fixed", keep: true })));
    } catch (e) {
      setError((e as { message: string }).message);
    }
  }, [chatId, upto]);

  useEffect(() => {
    draft();
    api.knowledgeInfo(serviceId).then((i) => setDir(i.dir)).catch(() => {});
  }, [draft, serviceId]);

  const edit = (i: number, patch: Partial<Card>) => setCards((cs) => cs && cs.map((c, j) => (j === i ? { ...c, ...patch } : c)));
  const chosen = (cards ?? []).filter((c) => c.keep);
  const valid = chosen.length > 0 && chosen.every((c) => c.title.trim() && c.markdown.trim());

  const save = async () => {
    setBusy(true);
    setSaveError(null);
    try {
      const saved = await api.saveKnowledge(serviceId, chatId, today(), chosen.map(({ title, markdown, status }) => ({ title: title.trim(), markdown, status })));
      toast(`บันทึกความรู้แล้ว ${saved.length} เรื่อง · กำลังทำ index`);
      onClose();
    } catch (e) {
      setSaveError((e as { message: string }).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title="บันทึกเป็นความรู้" onClose={onClose} width={760}>
      <p className="muted" style={{ fontSize: 13, lineHeight: 1.7, margin: 0 }}>
        ตรวจและแก้ร่างก่อนบันทึก บันทึกจะถูกใช้ตอบคำถามเรื่องเดียวกันครั้งหน้า (ถ้าขัดกับเอกสาร คำตอบจะยึดเอกสาร)
        {dir && <><br />เก็บที่ <span className="mono" style={{ wordBreak: "break-all" }}>{dir}</span></>}
      </p>

      {error && <ErrorState message={error} onRetry={draft} />}
      {!cards && !error && (
        <div style={{ display: "flex", flexDirection: "column", gap: 10 }} aria-busy="true" aria-label="กำลังร่างบันทึก">
          <span className="muted" style={{ fontSize: 13 }}>กำลังให้โมเดลเรียบเรียงบทสนทนา…</span>
          <Skeleton h={38} /><Skeleton h={160} />
        </div>
      )}
      {cards && cards.length > 1 && <span className="muted" style={{ fontSize: 13 }}>พบ {cards.length} เรื่อง — ติ๊กเฉพาะเรื่องที่จะเก็บ</span>}
      {cards?.map((c, i) => (
        <div key={i} className="note-card" data-off={!c.keep || undefined}>
          <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
            {cards.length > 1 && (
              <input type="checkbox" checked={c.keep} aria-label={`เก็บเรื่อง ${c.title}`} onChange={(e) => edit(i, { keep: e.target.checked })} style={{ width: 18, height: 18 }} />
            )}
            <input className="input" style={{ flex: 1, fontWeight: 600 }} value={c.title} maxLength={120} aria-label="ชื่อเรื่อง" disabled={!c.keep} onChange={(e) => edit(i, { title: e.target.value })} />
          </div>
          <textarea className="textarea mono" rows={12} value={c.markdown} aria-label={`เนื้อหาบันทึก ${c.title}`} disabled={!c.keep} onChange={(e) => edit(i, { markdown: e.target.value })} style={{ fontSize: 13, lineHeight: 1.6 }} />
          <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
            <span className="muted" style={{ fontSize: 13 }}>สถานะ</span>
            <div className="segmented" role="radiogroup" aria-label="สถานะ">
              {([["fixed", "แก้ได้แล้ว"], ["unverified", "ยังไม่ยืนยัน"]] as const).map(([v, label]) => (
                <button key={v} type="button" role="radio" aria-checked={c.status === v} disabled={!c.keep} onClick={() => edit(i, { status: v })}>{label}</button>
              ))}
            </div>
          </div>
        </div>
      ))}

      {saveError && <div className="err-box" role="alert">{saveError}</div>}
      <div className="modal-actions" style={{ justifyContent: "space-between" }}>
        <button type="button" className="btn btn-lg" onClick={() => api.openKnowledgeFolder(serviceId).catch((e) => toast((e as { message: string }).message))}>
          <FolderOpen size={16} strokeWidth={1.8} aria-hidden="true" /> เปิดโฟลเดอร์ความรู้{service ? ` (${service.code})` : ""}
        </button>
        <div style={{ display: "flex", gap: 10 }}>
          <button type="button" className="btn btn-lg" onClick={onClose}>{th.cancel}</button>
          <button type="button" className="btn btn-lg btn-primary" disabled={!valid || busy} onClick={save}>
            <BookMarked size={16} strokeWidth={1.8} aria-hidden="true" />
            {chosen.length > 1 ? `บันทึก ${chosen.length} เรื่อง` : "บันทึก"}
          </button>
        </div>
      </div>
    </Modal>
  );
}
