import { useEffect, useState } from "react";
import { api } from "../lib/api";
import type { UpdateInfo, UpdateProgress } from "../lib/types";
import { th } from "../i18n/th";
import { formatBytes } from "../state";
import { ProgressBar } from "./ProgressBar";
import { Modal } from "./ui";

/** Shows release notes, then downloads + installs (signature checked in Rust) and restarts. */
export function UpdateDialog({ info, onClose }: { info: UpdateInfo; onClose: () => void }) {
  const [progress, setProgress] = useState<UpdateProgress | null>(null);
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const off = api.on("update-progress", setProgress);
    return () => { off.then((f) => f()); };
  }, []);

  const install = async () => {
    setError(null);
    setInstalling(true);
    try {
      await api.installUpdate(); // the app restarts when done
    } catch (e) {
      setError((e as { message: string }).message);
      setInstalling(false);
    }
  };

  return (
    <Modal title={`อัปเดตเป็นเวอร์ชัน ${info.version}`} onClose={installing ? () => {} : onClose} width={520}>
      <div className="kv">
        <div><span className="k">เวอร์ชันปัจจุบัน</span><span className="mono">{info.currentVersion}</span></div>
        <div><span className="k">เวอร์ชันใหม่</span><span className="mono">{info.version}</span></div>
        {info.date && <div><span className="k">วันที่ออก</span><span>{info.date}</span></div>}
      </div>
      {info.notes && (
        <div className="info-box" style={{ whiteSpace: "pre-wrap", fontSize: 13, maxHeight: 220, overflowY: "auto" }} tabIndex={0} aria-label="รายการเปลี่ยนแปลง">
          <strong>มีอะไรใหม่</strong>
          {info.notes}
        </div>
      )}
      {installing && (
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }} role="status">
          <ProgressBar value={progress?.total ? progress.downloaded / progress.total : 0} label="ดาวน์โหลดอัปเดต" />
          <span className="field-hint">
            {progress?.done
              ? "กำลังติดตั้ง แอปจะปิดและเปิดใหม่เอง…"
              : `กำลังดาวน์โหลด ${formatBytes(progress?.downloaded ?? 0)}${progress?.total ? ` / ${formatBytes(progress.total)}` : ""}`}
          </span>
        </div>
      )}
      {error && <div className="err-box" role="alert">{error}</div>}
      <span className="field-hint">แชต เอกสาร การตั้งค่า และ Key จะยังอยู่ครบหลังอัปเดต</span>
      <div className="modal-actions">
        <button className="btn btn-lg" onClick={onClose} disabled={installing}>ภายหลัง</button>
        <button className="btn btn-lg btn-primary" onClick={install} disabled={installing}>
          {installing ? "กำลังอัปเดต…" : error ? th.tryAgain : "อัปเดตและเปิดใหม่"}
        </button>
      </div>
    </Modal>
  );
}
