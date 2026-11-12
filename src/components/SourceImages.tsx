import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";
import { api } from "../lib/api";
import type { ImageInfo, Source } from "../lib/types";
import { th } from "../i18n/th";

/** A picture kept from a document, with where it came from. */
export interface SourceImage {
  key: string;
  serviceId: string;
  info: ImageInfo;
  source: Source;
}

// data URLs are fetched once per picture per session
const urlCache = new Map<string, Promise<string>>();
const listCache = new Map<string, Promise<ImageInfo[]>>();

function imageUrl(serviceId: string, id: number): Promise<string> {
  const k = `${serviceId}:${id}`;
  let p = urlCache.get(k);
  if (!p) {
    p = api.imageData(serviceId, id);
    p.catch(() => urlCache.delete(k));
    urlCache.set(k, p);
  }
  return p;
}

function imagesOf(s: Source): Promise<ImageInfo[]> {
  const k = `${s.serviceId}|${s.docPath}|${s.page ?? ""}|${s.ord}`;
  let p = listCache.get(k);
  if (!p) {
    p = api.sourceImages(s).catch(() => [] as ImageInfo[]);
    listCache.set(k, p);
  }
  return p;
}

/** Pictures for a list of sources, de-duplicated, in source order. */
export function useSourceImages(sources: Source[], limit = 8): SourceImage[] {
  const [items, setItems] = useState<SourceImage[]>([]);
  const sig = sources.map((s) => `${s.serviceId}|${s.docPath}|${s.page}|${s.ord}`).join("\n");
  useEffect(() => {
    let alive = true;
    Promise.all(sources.map(imagesOf)).then((lists) => {
      if (!alive) return;
      const seen = new Set<string>();
      const out: SourceImage[] = [];
      lists.forEach((list, i) => {
        for (const info of list) {
          const key = `${sources[i].serviceId}:${info.id}`;
          if (seen.has(key) || out.length >= limit) continue;
          seen.add(key);
          out.push({ key, serviceId: sources[i].serviceId, info, source: sources[i] });
        }
      });
      setItems(out);
    });
    return () => { alive = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sig, limit]);
  return items;
}

function useImageUrl(serviceId: string, id: number): string | null {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    imageUrl(serviceId, id).then((u) => alive && setUrl(u)).catch(() => alive && setUrl(null));
    return () => { alive = false; };
  }, [serviceId, id]);
  return url;
}

function caption(s: Source): string {
  return s.page != null ? `${s.fileName} · ${th.page(s.page)}` : s.fileName;
}

export function Thumb({ img, onOpen, size = 96 }: { img: SourceImage; onOpen: () => void; size?: number }) {
  const url = useImageUrl(img.serviceId, img.info.id);
  const ratio = img.info.w / Math.max(1, img.info.h);
  const w = Math.round(Math.min(size * 1.8, Math.max(size * 0.6, size * ratio)));
  return (
    <button
      className="thumb"
      style={{ width: w, height: size }}
      onClick={onOpen}
      title={caption(img.source)}
      aria-label={`ดูรูปจาก ${caption(img.source)}`}
    >
      {url ? <img src={url} alt="" draggable={false} /> : <span className="thumb-loading" aria-hidden="true" />}
    </button>
  );
}

/** Full-size view; ← → move between pictures, Esc closes. */
export function Lightbox({ items, index, onIndex, onClose }: {
  items: SourceImage[];
  index: number;
  onIndex: (i: number) => void;
  onClose: () => void;
}) {
  const img = items[index];
  const url = useImageUrl(img.serviceId, img.info.id);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.stopPropagation(); onClose(); }
      if (e.key === "ArrowRight" && index < items.length - 1) onIndex(index + 1);
      if (e.key === "ArrowLeft" && index > 0) onIndex(index - 1);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [index, items.length, onClose, onIndex]);

  return createPortal(
    <div className="lightbox" role="dialog" aria-modal="true" aria-label="รูปจากเอกสาร" onMouseDown={(e) => { e.stopPropagation(); if (e.target === e.currentTarget) onClose(); }}>
      <button className="icon-btn lightbox-close" aria-label="ปิด" onClick={onClose} autoFocus><X size={20} strokeWidth={1.8} /></button>
      {url ? <img src={url} alt={caption(img.source)} /> : <span className="thumb-loading" aria-hidden="true" />}
      <div className="lightbox-bar">
        <span className="ellipsis">{caption(img.source)}</span>
        {items.length > 1 && <span className="mono">{index + 1}/{items.length}</span>}
      </div>
    </div>,
    document.body,
  );
}

/** Thumbnails of the pictures around one cited chunk (inside the source panel). */
export function SourceImageGrid({ source }: { source: Source }) {
  const items = useSourceImages([source], 12);
  const [open, setOpen] = useState<number | null>(null);
  if (!items.length) return null;
  return (
    <div className="thumbs" aria-label="รูปจากเอกสาร">
      {items.map((img, i) => <Thumb key={img.key} img={img} onOpen={() => setOpen(i)} size={110} />)}
      {open != null && <Lightbox items={items} index={open} onIndex={setOpen} onClose={() => setOpen(null)} />}
    </div>
  );
}

// picture ids change when a document is re-indexed → forget the lists
api.on("index-progress", (p) => { if (p.phase === "done") listCache.clear(); }).catch(() => {});
