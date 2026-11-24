import { useState } from "react";
import { ChevronDown, ChevronRight, FilePenLine, FilePlus2, Undo2 } from "lucide-react";
import type { CodeChange } from "../lib/types";

type Line = { t: "ctx" | "add" | "del"; s: string };

/** A minimal line diff (LCS) for showing what a coding turn changed. Big files skip the diff. */
function lineDiff(before: string | null, after: string): Line[] {
  const a = before === null ? [] : before.split("\n");
  const b = after.split("\n");
  if (a.length > 800 || b.length > 800) {
    return b.map((s) => ({ t: "add", s }));
  }
  const n = a.length;
  const m = b.length;
  const dp: number[][] = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  const out: Line[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      out.push({ t: "ctx", s: a[i] });
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      out.push({ t: "del", s: a[i] });
      i++;
    } else {
      out.push({ t: "add", s: b[j] });
      j++;
    }
  }
  while (i < n) out.push({ t: "del", s: a[i++] });
  while (j < m) out.push({ t: "add", s: b[j++] });
  return out;
}

const lineBg: Record<Line["t"], string> = {
  add: "color-mix(in srgb, #22c55e 18%, transparent)",
  del: "color-mix(in srgb, #ef4444 16%, transparent)",
  ctx: "transparent",
};
const linePrefix: Record<Line["t"], string> = { add: "+", del: "-", ctx: " " };

function Diff({ change }: { change: CodeChange }) {
  const lines = lineDiff(change.before, change.after);
  return (
    <pre className="code-diff" style={{ margin: 0, padding: "6px 0", overflowX: "auto", fontSize: 12.5, lineHeight: 1.55, fontFamily: "var(--mono, ui-monospace, monospace)" }}>
      {lines.map((l, i) => (
        <div key={i} style={{ background: lineBg[l.t], padding: "0 10px", whiteSpace: "pre" }}>
          <span aria-hidden="true" style={{ opacity: 0.5, userSelect: "none" }}>{linePrefix[l.t]} </span>
          {l.s || " "}
        </div>
      ))}
    </pre>
  );
}

function FileRow({ change }: { change: CodeChange }) {
  const [open, setOpen] = useState(false);
  const created = change.kind === "create";
  return (
    <div className="change-file" style={{ border: "1px solid var(--border)", borderRadius: 8, overflow: "hidden" }}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        style={{ display: "flex", alignItems: "center", gap: 8, width: "100%", padding: "7px 10px", background: "transparent", border: 0, cursor: "pointer", font: "inherit", color: "inherit", textAlign: "left" }}
      >
        {open ? <ChevronDown size={14} strokeWidth={2} aria-hidden="true" /> : <ChevronRight size={14} strokeWidth={2} aria-hidden="true" />}
        {created ? <FilePlus2 size={14} strokeWidth={2} aria-hidden="true" style={{ color: "#22c55e" }} /> : <FilePenLine size={14} strokeWidth={2} aria-hidden="true" style={{ color: "var(--accent)" }} />}
        <span className="ellipsis" style={{ flex: 1, fontSize: 13 }}>{change.path}</span>
        <span style={{ fontSize: 11, opacity: 0.7 }}>{created ? "สร้าง" : "แก้"}</span>
      </button>
      {open && <div style={{ borderTop: "1px solid var(--border)" }}><Diff change={change} /></div>}
    </div>
  );
}

/** The files a coding turn changed, with per-file diffs and (for a saved message) an undo button. */
export function ChangesList({ changes, onUndo, undoing, undone }: { changes: CodeChange[]; onUndo?: () => void; undoing?: boolean; undone?: boolean }) {
  if (changes.length === 0) return null;
  return (
    <div className="changes-list" style={{ display: "flex", flexDirection: "column", gap: 6, margin: "6px 0 10px" }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <span style={{ fontSize: 12.5, opacity: 0.8 }}>{undone ? "ย้อนกลับแล้ว — " : ""}แก้ไฟล์ {changes.length} รายการ</span>
        <span style={{ flex: 1 }} />
        {onUndo && !undone && (
          <button type="button" className="scope-btn" onClick={onUndo} disabled={undoing} title="คืนไฟล์ทั้งหมดกลับเป็นก่อนการแก้ครั้งนี้ (ลบไฟล์ที่เพิ่งสร้าง)">
            <Undo2 size={13} strokeWidth={2} aria-hidden="true" />
            {undoing ? "กำลังย้อน…" : "ย้อนกลับ"}
          </button>
        )}
      </div>
      {changes.map((c) => (
        <FileRow key={c.path} change={c} />
      ))}
    </div>
  );
}
