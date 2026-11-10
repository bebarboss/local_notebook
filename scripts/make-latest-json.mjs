// Builds latest.json for tauri-plugin-updater from the signed installers.
// Usage: node scripts/make-latest-json.mjs <baseUrl> ["release notes"]
//   baseUrl = where you will upload the installers, e.g. https://files.example.internal/docask
//   Notes may also come from env DOCASK_RELEASE_NOTES (safer for Thai text from PowerShell 5.1).
import { readFileSync, writeFileSync, existsSync, mkdirSync, copyFileSync } from "node:fs";
import { join } from "node:path";

const [, , baseUrlArg, notesArg] = process.argv;
if (!baseUrlArg || !baseUrlArg.startsWith("https://")) {
  console.error("usage: node scripts/make-latest-json.mjs https://<server>/<folder> [\"notes\"]");
  process.exit(1);
}
const baseUrl = baseUrlArg.replace(/\/+$/, "");
const version = JSON.parse(readFileSync("package.json", "utf8")).version;
const bundle = join("src-tauri", "target", "release", "bundle");

const files = {
  nsis: join(bundle, "nsis", `DocAsk_${version}_x64-setup.exe`),
  msi: join(bundle, "msi", `DocAsk_${version}_x64_en-US.msi`),
};
const platforms = {};
const out = join("release", version);
mkdirSync(out, { recursive: true });

for (const [kind, file] of Object.entries(files)) {
  const sig = `${file}.sig`;
  if (!existsSync(file) || !existsSync(sig)) {
    console.error(`missing ${file} or its .sig — build with the signing key set (see README §11)`);
    process.exit(1);
  }
  const name = file.split(/[\\/]/).pop();
  copyFileSync(file, join(out, name));
  platforms[`windows-x86_64-${kind}`] = { signature: readFileSync(sig, "utf8").trim(), url: `${baseUrl}/${encodeURIComponent(name)}` };
}
// default for clients that don't report an installer type: per-user NSIS
platforms["windows-x86_64"] = platforms["windows-x86_64-nsis"];

const latest = {
  version,
  notes: process.env.DOCASK_RELEASE_NOTES || notesArg || `DocAsk ${version}`,
  pub_date: new Date().toISOString(),
  platforms,
};
writeFileSync(join(out, "latest.json"), JSON.stringify(latest, null, 2));
console.log(`release/${version}/ ready — upload everything in it to ${baseUrl}/`);
console.log(`clients must point their update URL at ${baseUrl}/latest.json`);
