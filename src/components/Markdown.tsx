// Minimal, safe markdown subset for answers: paragraphs, **bold**, `code`, ```fences```,
// bullet / numbered lists, | tables |, and [n] citations rendered as CitationChip. No raw HTML.
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

type Align = "left" | "center" | "right" | undefined;

type Block =
  | { t: "p"; text: string }
  | { t: "code"; text: string }
  | { t: "ul" | "ol"; items: string[] }
  | { t: "h"; text: string }
  | { t: "table"; head: string[]; align: Align[]; rows: string[][] };

/** `| a | b |` → ["a", "b"]; a `|` inside `code` or written as `\|` stays in the cell */
function splitRow(line: string): string[] {
  let s = line.trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|") && !s.endsWith("\\|")) s = s.slice(0, -1);
  const cells: string[] = [];
  let cur = "";
  let inCode = false;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (c === "\\" && s[i + 1] === "|") { cur += "|"; i++; continue; }
    if (c === "`") inCode = !inCode;
    if (c === "|" && !inCode) { cells.push(cur.trim()); cur = ""; continue; }
    cur += c;
  }
  cells.push(cur.trim());
  return cells;
}

/** the `|---|:---:|` line under a table header */
function separator(line: string): Align[] | null {
  if (!line.includes("-")) return null;
  const cells = splitRow(line);
  if (!cells.every((c) => /^:?-{1,}:?$/.test(c))) return null;
  return cells.map((c) => (c.startsWith(":") && c.endsWith(":") ? "center" : c.endsWith(":") ? "right" : c.startsWith(":") ? "left" : undefined));
}

export function parseBlocks(src: string): Block[] {
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
    // a table: a row with "|", then the |---| line
    const align = line.includes("|") && i + 1 < lines.length ? separator(lines[i + 1]) : null;
    if (align) {
      flush();
      const head = splitRow(line);
      const rows: string[][] = [];
      i += 2;
      while (i < lines.length && lines[i].includes("|") && lines[i].trim()) {
        const r = splitRow(lines[i++]);
        // ragged rows from the model: pad / cut to the header's width
        rows.push(head.map((_, j) => r[j] ?? ""));
      }
      i--;
      out.push({ t: "table", head, align: head.map((_, j) => align[j]), rows });
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
          case "table": return (
            <Fragment key={i}>
              <div className="md-table" role="region" aria-label="ตาราง" tabIndex={0}>
                <table>
                  <thead>
                    <tr>{b.head.map((c, j) => <th key={j} style={{ textAlign: b.align[j] }}>{inline(c, sources, `t${i}-h${j}`)}</th>)}</tr>
                  </thead>
                  <tbody>
                    {b.rows.map((r, ri) => (
                      <tr key={ri}>{r.map((c, j) => <td key={j} style={{ textAlign: b.align[j] }}>{inline(c, sources, `t${i}-${ri}-${j}`)}</td>)}</tr>
                    ))}
                  </tbody>
                </table>
              </div>
              {tail}
            </Fragment>
          );
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
