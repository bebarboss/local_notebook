import { useState } from "react";
import type { Source } from "../lib/types";
import { SourcePanel } from "./SourcePanel";

/** `[n]` inside an answer → small accent-soft chip; click shows the original chunk. */
export function CitationChip({ n, source }: { n: number; source: Source | undefined }) {
  const [open, setOpen] = useState(false);
  if (!source) return <span className="cite" aria-label={`แหล่งอ้างอิง ${n} (ไม่พบ)`}>{n}</span>;
  return (
    <>
      <button className="cite" aria-label={`แหล่งอ้างอิง ${n}: ${source.fileName}`} onClick={() => setOpen(true)}>{n}</button>
      {open && <SourcePanel source={source} onClose={() => setOpen(false)} />}
    </>
  );
}

export function sourceLabel(s: Source, showService: boolean): string {
  return [String(s.n), showService ? s.serviceName.split(" · ")[0] : null, s.fileName, s.page != null ? `น.${s.page}` : null]
    .filter(Boolean)
    .join(" · ");
}
