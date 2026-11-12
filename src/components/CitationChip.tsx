import { useState } from "react";
import { BookMarked } from "lucide-react";
import type { Source } from "../lib/types";
import { SourcePanel } from "./SourcePanel";

/** `[n]` inside an answer → small accent-soft chip; click shows the original chunk. */
export function CitationChip({ n, source }: { n: number; source: Source | undefined }) {
  const [open, setOpen] = useState(false);
  if (!source) return <span className="cite" aria-label={`แหล่งอ้างอิง ${n} (ไม่พบ)`}>{n}</span>;
  return (
    <>
      {source.knowledge ? (
        <button className="cite note" aria-label={`แหล่งอ้างอิง ${n}: บันทึกความรู้`} title="บันทึกความรู้จากแชตก่อน" onClick={() => setOpen(true)}>
          <BookMarked size={10} strokeWidth={2.2} aria-hidden="true" />{n}
        </button>
      ) : (
        <button className="cite" aria-label={`แหล่งอ้างอิง ${n}: ${source.fileName}`} onClick={() => setOpen(true)}>{n}</button>
      )}
      {open && <SourcePanel source={source} onClose={() => setOpen(false)} />}
    </>
  );
}
