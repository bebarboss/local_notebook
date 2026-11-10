// Downloads the OCR models (PaddleOCR → ONNX) and the PDFium library that are
// bundled into the installer. Runs automatically before `tauri dev` / `tauri build`.
// Files land in src-tauri/resources/ (git-ignored). Already-present files are kept.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, writeFileSync, readFileSync, rmSync, copyFileSync } from "node:fs";
import { join } from "node:path";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";

// pinned versions — bump deliberately
const HF = "https://huggingface.co/monkt/paddleocr-onnx/resolve/7b02d0a30a07ba2b92ad1ff5a8941ae2c633de65";
const PDFIUM = "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F8125/pdfium-win-x64.tgz";

const root = join("src-tauri", "resources");
const files = [
  // PP-OCRv3 mobile text detection (2.3 MB)
  { url: `${HF}/detection/v3/det.onnx`, dest: join(root, "ocr", "det.onnx") },
  // PP-OCRv5 Thai recognition — Thai + English (7.5 MB)
  { url: `${HF}/languages/thai/rec.onnx`, dest: join(root, "ocr", "rec.onnx") },
  { url: `${HF}/languages/thai/dict.txt`, dest: join(root, "ocr", "dict.txt") },
];

async function download(url) {
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) throw new Error(`${res.status} ${url}`);
  return Buffer.from(await res.arrayBuffer());
}

let fetched = 0;
for (const f of files) {
  if (existsSync(f.dest)) continue;
  mkdirSync(join(f.dest, ".."), { recursive: true });
  process.stdout.write(`fetch ${f.dest} … `);
  const buf = await download(f.url);
  writeFileSync(f.dest, buf);
  console.log(`${(buf.length / 1e6).toFixed(1)} MB sha256:${createHash("sha256").update(buf).digest("hex").slice(0, 12)}`);
  fetched++;
}

const dll = join(root, "pdfium", "pdfium.dll");
if (!existsSync(dll) && process.platform === "win32") {
  mkdirSync(join(root, "pdfium"), { recursive: true });
  process.stdout.write(`fetch ${dll} … `);
  const tgz = join(tmpdir(), `pdfium-${Date.now()}.tgz`);
  const out = join(tmpdir(), `pdfium-${Date.now()}`);
  writeFileSync(tgz, await download(PDFIUM));
  mkdirSync(out, { recursive: true });
  execFileSync("tar", ["-xzf", tgz, "-C", out]);
  copyFileSync(join(out, "bin", "pdfium.dll"), dll);
  const lic = join(out, "LICENSE");
  if (existsSync(lic)) copyFileSync(lic, join(root, "pdfium", "LICENSE"));
  rmSync(tgz, { force: true });
  rmSync(out, { recursive: true, force: true });
  console.log(`${(readFileSync(dll).length / 1e6).toFixed(1)} MB`);
  fetched++;
}

console.log(fetched ? `OCR resources ready (${fetched} downloaded)` : "OCR resources already present");
