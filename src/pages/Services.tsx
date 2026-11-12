import { useState } from "react";
import { ArrowLeft, FileArchive, Plus } from "lucide-react";
import { api } from "../lib/api";
import type { Service } from "../lib/types";
import { useApp } from "../state";
import { ServiceCard } from "../components/ServiceCard";
import { ServiceEditor } from "../components/ServiceEditor";
import { ConfirmDialog, SkeletonList } from "../components/ui";

export default function ServicesPage() {
  const app = useApp();
  const { services, stats, go, activeServiceId, setActiveServiceId, ready, toast } = app;
  const [editing, setEditing] = useState<Service | "new" | null>(null);
  const [importing, setImporting] = useState<number | null>(null);
  const [mismatch, setMismatch] = useState<{ zip: string; dest: string; message: string } | null>(null);

  const runImport = async (zip: string, dest: string, reembed: boolean) => {
    setImporting(0);
    const off = api.on("share-progress", (p) => p.phase === "import" && setImporting(p.total ? p.done / p.total : 0));
    try {
      const s = await api.importServiceZip(zip, dest, reembed);
      app.refreshServices();
      setActiveServiceId(s.id);
      toast(reembed ? `นำเข้า ${s.name} แล้ว — กำลังทำ index ในเครื่องนี้` : `นำเข้า ${s.name} แล้ว — ถามได้เลย เอกสารอยู่ที่ ${s.folders[0]}`);
    } catch (e) {
      const err = e as { code?: string; message: string };
      if (err.code === "embedding_mismatch") setMismatch({ zip, dest, message: err.message });
      else toast(err.message);
    } finally {
      setImporting(null);
      off.then((f) => f());
    }
  };

  const importZip = async () => {
    const zip = await api.pickZip();
    if (!zip) return;
    toast("เลือกโฟลเดอร์ที่จะเก็บเอกสารของ service นี้");
    const dest = await api.pickFolder();
    if (!dest) return;
    await runImport(zip, dest, false);
  };

  const addDocs = async (s: Service) => {
    const files = await api.pickFiles();
    if (!files.length) return;
    try {
      await api.addDocuments(s.id, files);
      toast(`เพิ่ม ${files.length} ไฟล์ใน ${s.code} กำลังทำ index`);
      app.refreshServices();
    } catch (e) { toast((e as { message: string }).message); }
  };

  return (
    <main className="page" aria-label="จัดการ Service">
      <div className="page-inner">
        <div className="page-head">
          <div className="l">
            <button className="back-link" onClick={() => go({ name: "newchat" })}><ArrowLeft size={14} strokeWidth={1.8} aria-hidden="true" />กลับไปแชต</button>
            <h1 className="page-title">Service</h1>
            <p className="muted">แยกเอกสารเป็นกลุ่ม แต่ละ service มี index และการตั้งค่าของตัวเอง ตอนถามจะค้นเฉพาะใน service ที่เลือก</p>
          </div>
          <div style={{ display: "flex", gap: 10, flexWrap: "wrap" }}>
            <button className="btn btn-lg" disabled={importing != null} onClick={importZip}
              title="เปิดไฟล์ .zip ที่ส่งออกจาก DocAsk (index + เอกสาร) แล้วใช้ได้ทันทีโดยไม่ต้อง index ใหม่">
              <FileArchive size={16} strokeWidth={1.8} aria-hidden="true" />
              {importing != null ? `กำลังนำเข้า ${Math.round(importing * 100)}%` : "นำเข้า service (.zip)"}
            </button>
            <button className="btn btn-primary btn-lg" onClick={() => setEditing("new")}>
              <Plus size={16} strokeWidth={2.2} aria-hidden="true" /> สร้าง service ใหม่
            </button>
          </div>
        </div>

        {!ready ? (
          <SkeletonList rows={2} h={220} />
        ) : (
          <div className="svc-grid">
            {services.map((s) => (
              <ServiceCard
                key={s.id}
                service={s}
                stats={stats[s.id]}
                active={s.id === activeServiceId}
                onOpen={() => { setActiveServiceId(s.id); go({ name: "newchat" }); }}
                onSettings={() => setEditing(s)}
                onAddDocs={() => addDocs(s)}
              />
            ))}
            <button className="svc-new" onClick={() => setEditing("new")}>
              <Plus size={24} strokeWidth={1.8} aria-hidden="true" />
              <span style={{ fontWeight: 600 }}>สร้าง service ใหม่</span>
              <span style={{ fontSize: 12 }}>เลือกโฟลเดอร์ ตั้งชื่อ และเลือกโมเดลเริ่มต้น</span>
            </button>
          </div>
        )}

        <div className="info-box" style={{ fontSize: 13, padding: "14px 16px" }}>
          แต่ละ service มีฐาน vector แยกไฟล์กัน ลบหรือส่งออกทีละ service ได้ (ส่งออกเป็น .zip พร้อมเอกสาร ผู้รับกด "นำเข้า service" แล้วถามได้ทันที) และคำถามใน service หนึ่งจะไม่ค้นเจอเอกสารของ service อื่น ยกเว้นเปิด "ค้นทุก service" ในช่องพิมพ์
        </div>
      </div>

      {mismatch && (
        <ConfirmDialog
          title="โมเดล embedding ไม่ตรงกัน"
          body={mismatch.message}
          confirmLabel="นำเข้าแล้ว index ใหม่"
          onCancel={() => setMismatch(null)}
          onConfirm={() => { const m = mismatch; setMismatch(null); void runImport(m.zip, m.dest, true); }}
        />
      )}
      {editing && (
        <ServiceEditor
          service={editing === "new" ? null : editing}
          onClose={(saved) => {
            setEditing(null);
            if (saved && editing === "new") { setActiveServiceId(saved.id); toast(`สร้าง ${saved.name} แล้ว กำลังทำ index`); }
          }}
        />
      )}
    </main>
  );
}
