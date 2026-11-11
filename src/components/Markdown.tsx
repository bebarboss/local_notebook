// Minimal, safe markdown subset for answers: paragraphs, **bold**, `code`, ```fences```,
// bullet / numbered lists, and [n] citations rendered as CitationChip. No raw HTML.
import { Fragment, useState, type ReactNode } from "react";
import { Check, Copy } from "lucide-react";
import type { Source } from "../lib/types";
import { CitationChip } from "./CitationChip";

/** Code / SQL from the documents, with a copy button (copies the code only). */
function CodeBlock({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="code-block">
      <button
        className="icon-btn code-copy"
        aria-label={copied ? "คัดลอกแล้ว" : "คัดลอกโค้ด"}
        title={copied ? "คัดลอกแล้ว" : "คัดลอกโค้ด"}
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(text);
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          } catch { /* clipboard unavailable */ }
        }}
      >
        {copied ? <Check size={14} strokeWidth={1.8} /> : <Copy size={14} strokeWidth={1.8} />}
      </button>
      <pre><code>{text}</code></pre>
    </div>
  );
}

type Block =
  | { t: "p"; text: string }
  | { t: "code"; text: string }
  | { t: "ul" | "ol"; items: string[] }
  | { t: "h"; text: string };

function parseBlocks(src: string): Block[] {
  const lines = src.replace(/\r\n/g, "\n").split("\n");
  const out: Block[] = [];
  let para: string[] = [];
  const flush = () => { if (para.length) { out.push({ t: "p", text: para.join("\n") }); para = []; } };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (line.trimStart().startsWith("```")) {
      flush();
      const buf: string[] = [];
      i++;
      while (i < lines.length && !lines[i].trimStart().startsWith("```")) buf.push(lines[i++]);
      out.push({ t: "code", text: buf.join("\n") });
      continue;
    }
    const ul = /^\s*[-*•]\s+(.*)$/.exec(line);
    const ol = /^\s*\d+[.)]\s+(.*)$/.exec(line);
    if (ul || ol) {
      flush();
      const kind = ul ? "ul" : "ol";
      const last = out[out.length - 1];
      const item = (ul ?? ol)![1];
      if (last && last.t === kind) last.items.push(item);
      else out.push({ t: kind, items: [item] });
      continue;
    }
    const h = /^#{1,4}\s+(.*)$/.exec(line);
    if (h) { flush(); out.push({ t: "h", text: h[1] }); continue; }
    if (!line.trim()) { flush(); continue; }
    para.push(line);
  }
  flush();
  return out;
}

function inline(text: string, sources: Source[], key: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  const re = /(\*\*[^*]+\*\*|`[^`]+`|\[\d{1,3}\](?:\[\d{1,3}\])*)/g;
  let last = 0;
  let m: RegExpExecArray | null;
  let k = 0;
  while ((m = re.exec(text))) {
    if (m.index > last) nodes.push(text.slice(last, m.index));
    const tok = m[0];
    if (tok.startsWith("**")) nodes.push(<strong key={`${key}-${k++}`}>{tok.slice(2, -2)}</strong>);
    else if (tok.startsWith("`")) nodes.push(<code key={`${key}-${k++}`}>{tok.slice(1, -1)}</code>);
    else {
      for (const nm of tok.matchAll(/\[(\d{1,3})\]/g)) {
        const n = Number(nm[1]);
        nodes.push(<CitationChip key={`${key}-${k++}`} n={n} source={sources.find((s) => s.n === n)} />);
      }
    }
    last = m.index + tok.length;
  }
  if (last < text.length) nodes.push(text.slice(last));
  // keep single newlines inside a paragraph
  return nodes.flatMap((n, i): ReactNode[] =>
    typeof n === "string" ? n.split("\n").flatMap((part, j, arr): ReactNode[] => (j < arr.length - 1 ? [part, <br key={`${key}-br-${i}-${j}`} />] : [part])) : [n],
  );
}

export function Markdown({ text, sources, trailing }: { text: string; sources: Source[]; trailing?: ReactNode }) {
  const blocks = parseBlocks(text);
  return (
    <div className="ai-text">
      {blocks.map((b, i) => {
        const tail = i === blocks.length - 1 ? trailing : null;
        switch (b.t) {
          case "p": return <p key={i}>{inline(b.text, sources, `p${i}`)}{tail}</p>;
          case "h": return <p key={i}><strong>{inline(b.text, sources, `h${i}`)}</strong>{tail}</p>;
          case "code": return <Fragment key={i}><CodeBlock text={b.text} />{tail}</Fragment>;
          default: {
            const L = b.t;
            return (
              <L key={i}>
                {b.items.map((it, j) => <li key={j}>{inline(it, sources, `l${i}-${j}`)}{j === b.items.length - 1 ? tail : null}</li>)}
              </L>
            );
          }
        }
      })}
      {blocks.length === 0 && trailing}
    </div>
  );
}
