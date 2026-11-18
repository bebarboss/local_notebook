// "มีบันทึกเรื่องนี้อยู่แล้ว": the new note and a similar one side by side. "รวม" lets the model
// write one note that keeps every fix (each marked ได้ผล / ยังไม่ยืนยัน / ไม่ได้ผล); the user
// checks it, and the two old notes are hidden (moved aside), never deleted. "เก็บทั้งคู่" keeps
// both and does not ask about this pair again.
import { useCallback, useEffect, useRef, useState } from "react";
import { BookMarked, GitMerge } from "lucide-react";
import { api } from "../lib/api";
import type { KnowledgeDuplicate, NewNote, SimilarNote } from "../lib/types";
import { th } from "../i18n/th";
import { useApp } from "../state";
import { ErrorState, Modal, Skeleton } from "./ui";
import { Markdown } from "./Markdown";
import { ProgressBar } from "./ProgressBar";
import { draftProgress } from "./KnowledgeDialog";

type Phase = "compare" | "merging" | "edit";

function today(): string {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

function NoteColumn({ label, title, text, error }: { label: string; title: string; text: string | null; error: string | null }) {
  return (
    <div className="note-compare-col">
      <span className="muted" style={{ fontSize: 12 }}>{label}</span>
      <strong style={{ fontSize: 14 }}>{title}</strong>
      {error ? <div className="err-box" role="alert">{error}</div> : text == null ? <Skeleton h={160} /> : (
        <div className="note-compare-body"><Markdown text={text} sources={[]} /></div>
      )}
    </div>
  );
}

export function KnowledgeCompareDialog({ dup, other, onClose }: { dup: KnowledgeDuplicate; other: SimilarNote; onClose: (done: boolean) => void }) {
  const { toast } = useApp();
  const [texts, setTexts] = useState<[string | null, string | null]>([null, null]);
  const [readErr, setReadErr] = useState<[string | null, string | null]>([null, null]);
  const [phase, setPhase] = useState<Phase>("compare");
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<NewNote | null>(null);
  const [busy, setBusy] = useState(false);
  const [written, setWritten] = useState(0);
  const [seconds, setSeconds] = useState(0);
  const run = useRef(0);

  useEffect(() => {
    api.readNote(dup.serviceId, dup.path).then((t) => setTexts((x) => [t, x[1]])).catch((e) => setReadErr((x) => [(e as { message: string }).message, x[1]]));
    api.readNote(dup.serviceId, other.path).then((t) => setTexts((x) => [x[0], t])).catch((e) => setReadErr((x) => [x[0], (e as { message: string }).message]));
  }, [dup.serviceId, dup.path, other.path]);

  // progress while the model merges; closing the dialog stops it
  useEffect(() => {
    const off = api.on("knowledge-progress", setWritten);
    return () => { off.then((f) => f()); api.cancelKnowledgeDraft().catch(() => {}); };
  }, []);
  useEffect(() => {
    if (phase !== "merging" || error) return;
    const t = setInterval(() => setSeconds((s) => s + 1), 1000);
    return () => clearInterval(t);
  }, [phase, error]);

  const merge = useCallback(async () => {
    const mine = ++run.current;
    setPhase("merging");
    setError(null);
    setWritten(0);
    setSeconds(0);
    try {
      const d = await api.mergeNotes(dup.serviceId, dup.path, other.path);
      if (mine === run.current) {
        setNote({ ...d, status: "fixed" });
        setPhase("edit");
      }
    } catch (e) {
      const err = e as { code?: string; message: string };
      if (mine === run.current && err.code !== "cancelled") setError(err.message);
    }
  }, [dup.serviceId, dup.path, other.path]);

  const keepBoth = async () => {
    try {
      await api.keepNotesApart(dup.path, other.path);
      toast("เก็บไว้ทั้งสองบันทึก — จะไม่ถามคู่นี้อีก");
      onClose(true);
    } catch (e) { toast((e as { message: string }).message); }
  };

  const save = async () => {
    if (!note) return;
    setBusy(true);
    setError(null);
    try {
      await api.saveMergedNote(dup.serviceId, today(), { ...note, title: note.title.trim() }, [
        { path: dup.path },
        { path: other.path, documentId: other.documentId },
      ]);
      toast("รวมบันทึกแล้ว · บันทึกเดิมสองอันถูกซ่อน (ย้ายไปโฟลเดอร์ \"ซ่อนแล้ว\") · กำลังทำ index");
      onClose(true);
    } catch (e) {
      setError((e as { message: string }).message);
    } finally {
      setBusy(false);
    }
  };

  const pct = Math.round(other.similarity * 100);
  const ready = texts[0] != null && texts[1] != null;

  return (
    <Modal title="มีบันทึกเรื่องนี้อยู่แล้ว" onClose={() => onClose(false)} width={980}>
      {phase === "compare" && (
        <>
          <p className="muted" style={{ fontSize: 13, margin: 0 }}>
            สองบันทึกนี้คล้ายกัน {pct}% · <b>รวม</b> ให้ AI เขียนเป็นบันทึกเดียวที่เก็บวิธีแก้ทุกแบบพร้อมสถานะ (คุณตรวจแก้ก่อนบันทึก บันทึกเดิมถูกซ่อน ไม่ได้ลบ) ·
            <b> เก็บทั้งคู่</b> ถ้าเป็นคนละเรื่องกัน
          </p>
          <div className="note-compare">
            <NoteColumn label="บันทึกใหม่" title={dup.title} text={texts[0]} error={readErr[0]} />
            <NoteColumn label={`บันทึกเดิม · คล้าย ${pct}%`} title={other.title} text={texts[1]} error={readErr[1]} />
          </div>
          <div className="modal-actions">
            <button type="button" className="btn btn-lg" onClick={keepBoth}>เก็บทั้งคู่</button>
            <button type="button" className="btn btn-lg btn-primary" disabled={!ready} onClick={merge}>
              <GitMerge size={16} strokeWidth={1.8} aria-hidden="true" /> รวม
            </button>
          </div>
        </>
      )}

      {phase === "merging" && (error ? <ErrorState message={error} onRetry={merge} /> : (
        <div style={{ display: "flex", flexDirection: "column", gap: 10 }} aria-busy="true" aria-label="กำลังรวมบันทึก">
          <span className="muted" style={{ fontSize: 13 }} aria-live="polite">
            {written > 0 ? `โมเดลกำลังเขียนบันทึกที่รวมแล้ว… ${written.toLocaleString()} ตัวอักษร` : "กำลังให้โมเดลอ่านสองบันทึก…"} · {seconds} วินาที
          </span>
          <ProgressBar value={draftProgress(seconds, written)} label="ความคืบหน้าการรวมบันทึก" />
          <span className="muted" style={{ fontSize: 12 }}>ปิดหน้าต่างนี้ได้ทุกเมื่อ (หยุดทันที บันทึกเดิมไม่เปลี่ยน)</span>
        </div>
      ))}

      {phase === "edit" && note && (
        <>
          <p className="muted" style={{ fontSize: 13, margin: 0 }}>
            ตรวจแก้ก่อนบันทึก · วิธีแก้แต่ละข้อมีสถานะ (ได้ผล) (ยังไม่ยืนยัน) หรือ (ไม่ได้ผล) · บันทึกแล้วบันทึกเดิมสองอันจะถูกซ่อน
          </p>
          <div className="note-card">
            <input className="input" style={{ fontWeight: 600 }} value={note.title} maxLength={120} aria-label="ชื่อเรื่อง" onChange={(e) => setNote({ ...note, title: e.target.value })} />
            <textarea className="textarea mono" rows={16} value={note.markdown} aria-label="เนื้อหาบันทึกที่รวมแล้ว" onChange={(e) => setNote({ ...note, markdown: e.target.value })} style={{ fontSize: 13, lineHeight: 1.6 }} />
            <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
              <span className="muted" style={{ fontSize: 13 }}>สถานะทั้งบันทึก</span>
              <div className="segmented" role="radiogroup" aria-label="สถานะทั้งบันทึก">
                {([["fixed", "แก้ได้แล้ว"], ["unverified", "ยังไม่ยืนยัน"]] as const).map(([v, label]) => (
                  <button key={v} type="button" role="radio" aria-checked={note.status === v} onClick={() => setNote({ ...note, status: v })}>{label}</button>
                ))}
              </div>
            </div>
          </div>
          {error && <div className="err-box" role="alert">{error}</div>}
          <div className="modal-actions">
            <button type="button" className="btn btn-lg" onClick={() => onClose(false)}>{th.cancel}</button>
            <button type="button" className="btn btn-lg btn-primary" disabled={busy || !note.title.trim() || !note.markdown.trim()} onClick={save}>
              <BookMarked size={16} strokeWidth={1.8} aria-hidden="true" /> บันทึกที่รวมแล้ว
            </button>
          </div>
        </>
      )}
    </Modal>
  );
}
