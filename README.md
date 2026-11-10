# DocAsk

ถามตอบจากเอกสารในเครื่อง (RAG) ด้วย LLM ของบริษัทผ่าน **Gosoft Agent Gateway**
แอป desktop สำหรับ Windows x64 สร้างด้วย **Tauri v2** (Rust + React/TypeScript/Vite)

- เอกสารต้นฉบับและ vector อยู่ในเครื่องเท่านั้น (`%APPDATA%\DocAsk`) — ส่งออกไปเฉพาะท่อนที่ค้นเจอ + ประวัติแชตไปยัง Gateway ตอนถาม
- Embedding รันบน CPU (ONNX Runtime ผ่าน `fastembed`) ไม่ต้องมี GPU
- Virtual Key เก็บใน **Windows Credential Manager** เท่านั้น (ไม่อยู่ในไฟล์, log, frontend หรือ clipboard)

รายละเอียดความต้องการทั้งหมดอยู่ใน [SPEC.md](SPEC.md)

---

## 1. ติดตั้งเครื่องมือสำหรับพัฒนา (Windows)

| เครื่องมือ | วิธีติดตั้ง |
|---|---|
| Node.js 20 ขึ้นไป | `winget install OpenJS.NodeJS.LTS` |
| Rust (stable, MSVC) | `winget install Rustlang.Rustup` แล้วเปิด terminal ใหม่ `rustup default stable-msvc` |
| Visual Studio Build Tools 2022 (C++) | `winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"` |
| WebView2 Runtime | มีใน Windows 10/11 อยู่แล้ว |

> Build Tools ต้องมี workload **Desktop development with C++** และ Windows SDK
> (ONNX Runtime และ SQLite ถูก compile/link แบบ static ตอน build)

ตรวจสอบ:

```powershell
node -v; npm -v; cargo -V; rustc -V
```

## 2. คำสั่งที่ใช้บ่อย

```powershell
npm install              # ติดตั้ง dependency ฝั่ง frontend + Tauri CLI
npm run dev              # เปิดหน้าจอในเบราว์เซอร์ http://localhost:5173 (mock mode อัตโนมัติ)
npm run tauri dev        # แอปจริง + hot reload
npm run tauri build      # สร้างตัวติดตั้ง MSI + NSIS
cd src-tauri; cargo test # unit test ฝั่ง Rust
npm run typecheck        # ตรวจ TypeScript
```

ไฟล์ติดตั้งจะอยู่ที่

- `src-tauri\target\release\bundle\msi\DocAsk_<version>_x64_en-US.msi`
- `src-tauri\target\release\bundle\nsis\DocAsk_<version>_x64-setup.exe`

## 3. Mock mode (ดูหน้าจอโดยไม่ต้องมี Rust/VPN/Key)

ถ้า frontend ไม่ได้รันใน Tauri (เช่น `npm run dev` แล้วเปิดในเบราว์เซอร์) หรือ build ด้วย `DOCASK_MOCK=1`
`src/lib/api.ts` จะใช้ข้อมูลตัวอย่างใน `src/lib/mock.ts` แทนทุก command และแสดงป้าย **MOCK MODE** มุมขวาล่าง

- 4 services (HR, POS, จัดซื้อ, ประชุม) พร้อมเอกสารตัวอย่าง รวมไฟล์ที่กำลัง index และ PDF สแกนที่ถูกข้าม
- คำตอบแบบ streaming สมมติพร้อม `[1][2]`
- ผลทดสอบ 6 โมเดล (gothink3 ได้ 403)
- ไม่เรียกเครือข่ายใดๆ

ตัวช่วยสำหรับดูหน้าจอ (เฉพาะ mock):

- `http://localhost:5173/?setup=done&page=chat` — ข้าม wizard ไปหน้าแชต (`page` = `chat` | `services` | `settings` | `setup`)
- ใน DevTools console: `docaskMock.vpn(false)` จำลอง VPN หลุด, `docaskMock.vpn(true)` กลับมาปกติ

```powershell
npm run dev:mock         # บังคับ mock แม้รันใน Tauri
```

## 4. ตั้งค่าครั้งแรก (Setup Wizard)

แอปเปิด wizard 4 ขั้นอัตโนมัติ (เปิดซ้ำได้ที่ ตั้งค่า › เกี่ยวกับ › Setup Wizard)

1. **Provider** — Group / Endpoint Display Name `Gosoft Agent Gateway`, Gateway URL `https://agent-gateway.gosoft.co.th` (base API `/v1`, แก้ได้ + ปุ่ม "คืนค่าตามคู่มือ"), API Type `Chat Completions`
2. **Virtual Key** — ใส่ key ที่ได้จาก Admin (ขึ้นต้น `sk-`) แอปบันทึกลง Windows Credential Manager ชื่อ `DocAsk/Gosoft Agent Gateway`
   หลังบันทึกแอปแสดงแค่ `sk-••••` + 4 ตัวท้าย
   **ใช้ Virtual Key รายบุคคลเท่านั้น ห้ามใช้ LiteLLM Master Key และห้ามส่ง Screenshot ที่เห็น Key**
3. **โมเดล** — 6 โมเดลตามคู่มือ (`godev1–3`, `gothink1–3`) หรือกด **นำเข้าจาก VS Code** แล้วเลือก `chatLanguageModels.json`
   (แอปไม่อ่านฟิลด์ `apiKey` ในไฟล์นั้น) — ค่าเก็บที่ `%APPDATA%\DocAsk\models.json` ไม่มี key
4. **ทดสอบ** — ส่ง `Reply exactly: GOSOFT LITELLM OK` ทีละโมเดล โมเดลที่ไม่ผ่านจะถูกซ่อนไว้ก่อน

เปลี่ยน key ภายหลัง: ตั้งค่า › Gateway และ Key › **Update API Key** (ทดสอบซ้ำโดยไม่ต้องตั้ง 6 โมเดลใหม่)

## 5. โมเดล Embedding (ไม่แนบในตัวติดตั้ง)

ไปที่ ตั้งค่า › Embedding และ Index (แอปมีแถบเตือนจนกว่าจะตั้งค่า)

| โมเดล | ขนาดโดยประมาณ | หมายเหตุ |
|---|---|---|
| `multilingual-e5-small` (int8) | ~120 MB | ค่าเริ่มต้น เร็ว |
| `multilingual-e5-base` (int8) | ~280 MB | แม่นขึ้น |
| `bge-m3` (int8) | ~570 MB | แม่นที่สุด ใช้ RAM มาก |

**(ก) ดาวน์โหลดในแอป** — ดึงจาก Hugging Face (`Xenova/<model>`) พร้อมแถบความคืบหน้า เก็บที่ `%APPDATA%\DocAsk\models\<model>\`

**(ข) Offline / เครือข่ายบล็อก huggingface.co** — ดาวน์โหลดจากเครื่องอื่นแล้วคัดลอกมา จากนั้นกด "เลือกโฟลเดอร์"
โฟลเดอร์ต้องมีโครงสร้าง:

```
multilingual-e5-small\
  onnx\model_quantized.onnx      ← https://huggingface.co/Xenova/multilingual-e5-small/resolve/main/onnx/model_quantized.onnx
  tokenizer.json
  config.json
  special_tokens_map.json
  tokenizer_config.json
```

(ใช้ `model.onnx` แบบไม่ quantize ได้เช่นกัน) เปลี่ยนโมเดล, ขนาดท่อน หรือ overlap แล้ว แอปจะ index ใหม่ทั้งหมดในรอบถัดไป

### รุ่นที่แนบโมเดลในตัวติดตั้ง

วางโมเดลไว้ที่ `models\multilingual-e5-small\` (root ของ repo — ถูก `.gitignore` ไว้) แล้ว build ด้วย

```powershell
npm run tauri build -- --features bundled-model --config src-tauri/tauri.bundled-model.conf.json
```

## 6. การใช้งาน

1. **Service** (เมนูซ้าย › จัดการ) — สร้างกลุ่มเอกสาร: ชื่อ, รหัสย่อ 2–3 ตัว, สี, โฟลเดอร์ (หลายโฟลเดอร์ได้), โมเดลเริ่มต้น, reasoning effort, top-k, system prompt เพิ่มเติม
   แต่ละ service มี index ของตัวเอง `%APPDATA%\DocAsk\services\<id>\index.db` — ลบ/ส่งออก/index ใหม่ทีละ service ได้
2. แอปแปลง `.pdf .docx .txt .md` → ตัดท่อน (~1000 ตัวอักษร, overlap 150, ไม่ตัดกลางสระ/วรรณยุกต์) → embedding → เก็บใน SQLite
   ติดตามโฟลเดอร์อัตโนมัติ index เฉพาะไฟล์ใหม่/แก้ไข และลบท่อนของไฟล์ที่ถูกลบ · PDF สแกน (ไม่มีข้อความ) จะถูกข้าม
3. **แชตใหม่** — เลือก service ก่อน แล้วพิมพ์คำถาม แชตผูกกับ service นั้นตลอด (เปิด "ค้นทุก service" ได้ที่ช่องพิมพ์)
4. คำตอบแสดงแบบ streaming พร้อม `[n]` — กดเพื่อดูท่อนต้นฉบับ (ไฟล์, หน้า, คะแนน) และ "เปิดไฟล์ต้นฉบับ"
5. ถามต่อในแชตเดิมจะส่งคำถาม/คำตอบ 6 รอบล่าสุด (ตั้งค่าได้) — ไม่ส่งท่อนเอกสารของรอบเก่า และค้นเอกสารใหม่ทุกครั้ง

## 7. แก้ปัญหาที่พบบ่อย

| อาการ | ตรวจสอบ |
|---|---|
| ต่อ Gateway ไม่ได้ | ต่อ VPN บริษัทแล้วลองใหม่ · ตรวจ Gateway URL · แอปใช้ proxy ของ Windows และ certificate store ของเครื่องอัตโนมัติ |
| 401 | Virtual Key หมดอายุ ผิด หรือถูก revoke — กด Update API Key |
| 403 | ไม่มีสิทธิ์ใช้โมเดลนี้ ตรวจสิทธิ์ Model/Policy และชนิดของ Key กับ Admin |
| 404 | Gateway URL หรือชื่อโมเดล (Public Model ID) ไม่ถูกต้อง |
| 429 | เกินโควตา RPM, TPM หรือ budget — รอสักครู่หรือติดต่อ Admin |
| ไม่เห็นโมเดล | ทดสอบโมเดลอีกครั้ง (ตั้งค่า › Gateway › ทดสอบ) หรือ ตั้งค่า › โมเดล › "แสดงโมเดลที่ซ่อน" |
| ดาวน์โหลดโมเดลไม่ได้ | เครือข่ายบล็อก huggingface.co — ใช้วิธี (ข) เลือกโฟลเดอร์ |
| PDF ภาษาไทยอ่านออกมาเพี้ยน | เปลี่ยน `ingest/pdf.rs` เป็น `pdfium-render` (ใช้ trait `Extractor` เดิม) |
| `npm run tauri build` error เรื่อง `link.exe` / MSVC | ติดตั้ง VS Build Tools (C++) ตามข้อ 1 · ถ้ามี Git `link.exe` บัง PATH ให้เปิดผ่าน "Developer PowerShell for VS" |
| ลบข้อมูลทั้งหมด | ตั้งค่า › เกี่ยวกับ › ลบข้อมูลทั้งหมด (ลบ `%APPDATA%\DocAsk` และ key) — การถอนติดตั้งจะไม่ลบข้อมูลผู้ใช้ |

## 8. แพ็กและแจกจ่าย (สำหรับ IT)

- **NSIS** `DocAsk_x.y.z_x64-setup.exe` — ติดตั้งแบบ `currentUser` ไม่ต้องใช้สิทธิ์ admin มีภาษาไทย
- **MSI** — silent install: `msiexec /i DocAsk_x.y.z_x64_en-US.msi /qn`
- WebView2: `bundle.windows.webviewInstallMode` ตั้งเป็น `downloadBootstrapper` (เปลี่ยนเป็น `skip` ได้ถ้าเครื่องมีอยู่แล้ว) — ไม่ฝัง runtime
- Code signing: ใส่ `certificateThumbprint` และ `timestampUrl` ใน `src-tauri/tauri.conf.json` บนเครื่อง build ของ IT เท่านั้น
  **ห้าม commit ใบรับรองหรือรหัสผ่านลง repo**
- เวอร์ชันมาจาก `package.json` (tauri.conf.json อ้างถึง) แสดงที่ ตั้งค่า › เกี่ยวกับ

## 9. โครงสร้างโค้ด

```
src-tauri/src/
  main.rs, lib.rs          จุดเริ่มแอป, ลงทะเบียน command
  commands.rs              Tauri commands ทั้งหมด (+ RAG ask, prompt)
  config.rs                paths, settings, gateway, models.json, นำเข้าจาก VS Code
  secrets.rs               Virtual Key ↔ Windows Credential Manager (keyring)
  errors.rs, redact.rs     ข้อความ error ตาม SPEC §3.3, ตัด sk-... ออกทุกข้อความ
  llm.rs                   Chat Completions + SSE streaming (reqwest, rustls + Windows cert store)
  gateway_test.rs          ทดสอบ "GOSOFT LITELLM OK"
  ingest/                  Extractor trait, pdf/docx/text, chunker ภาษาไทย, watcher, indexer
  embed.rs                 fastembed (ONNX, CPU) + ดาวน์โหลด/เลือกโฟลเดอร์โมเดล
  store.rs, search.rs      SQLite ต่อ service + brute-force dot product (trait VectorStore)
  services.rs, chats.rs    service registry, ประวัติแชต
src/
  App.tsx, state.tsx, theme.css (design tokens), i18n/th.ts
  pages/      Chat, NewChat, Services, Setup, Settings
  components/ Sidebar, MessageList, Composer, CitationChip, SourcePanel, DocPanel, ServiceCard,
              ServiceEditor, ModelPicker, StatusPill, ProgressBar, TroubleshootTable, GatewayForms, Markdown, ui
  lib/        api.ts (invoke/listen wrapper), mock.ts, types.ts, gateway.ts
```

### Unit tests (Rust)

`cargo test` ครอบคลุม: chunker (ไทย/อังกฤษ/ผสม, ไม่ตัดกลางสระ-วรรณยุกต์), vector search, `redact()`,
การแยก index ต่อ service, ประวัติแชต (N รอบล่าสุด), การ parse SSE, การนำเข้า VS Code (ไม่อ่าน apiKey), prompt

## 10. ความปลอดภัย

- Frontend เรียกได้เฉพาะ Tauri IPC (CSP `connect-src ipc:`) ไม่มี HTTP ออกจาก frontend
- Capabilities: `core:default` + `dialog:allow-open` เท่านั้น — การอ่านไฟล์, เรียก Gateway และจัดการ key อยู่ใน Rust ทั้งหมด
- Frontend รู้แค่ "มี key แล้ว/ยังไม่มี" และ 4 ตัวท้าย · ช่องกรอก key ปิดการ copy/cut
- ทุกข้อความ error ผ่าน `redact()` (ตัด `sk-...`, `Bearer ...`, ฟิลด์ api_key ใน JSON)
- ไม่มี API key ใดๆ ใน repo, test, README หรือ config

## 11. อัปเดตโปรแกรม (in-app update)

แอปตรวจหาเวอร์ชันใหม่จากไฟล์ `latest.json` บน server ของบริษัท แล้วดาวน์โหลด ตรวจลายเซ็น ติดตั้ง และเปิดใหม่ให้เอง
ข้อมูลผู้ใช้ (แชต, index, การตั้งค่า, Key) ไม่หายหลังอัปเดต

**ฝั่งผู้ใช้**: แถบ "มี DocAsk เวอร์ชันใหม่" ขึ้นตอนเปิดแอป (ปิดได้ที่ ตั้งค่า › เกี่ยวกับ › ตรวจอัปเดตอัตโนมัติ)
หรือกด ตั้งค่า › เกี่ยวกับ › **ตรวจหาอัปเดต** · URL อัปเดตแก้ได้ในหน้าเดียวกัน (ต้องเป็น https)

### กุญแจเซ็นอัปเดต (ทำครั้งเดียว)

```powershell
npx tauri signer generate --ci -w "$env:USERPROFILE\.tauri\docask-updater.key"
```

- `docask-updater.key` = **กุญแจลับ** เก็บไว้กับคนที่ออก release เท่านั้น สำรองไว้ที่ปลอดภัย **ห้าม commit**
  ถ้าทำหาย เครื่องที่ติดตั้งไปแล้วจะรับอัปเดตไม่ได้อีก ต้องติดตั้งใหม่ด้วยมือ
- `docask-updater.key.pub` = กุญแจสาธารณะ ใส่ไว้ใน `src-tauri/tauri.conf.json` › `plugins.updater.pubkey` แล้ว
- แอปจะไม่ติดตั้งไฟล์ที่ไม่ได้เซ็นด้วยกุญแจนี้

### ออกเวอร์ชันใหม่

1. แก้ `"version"` ใน `package.json` (เช่น `0.1.0` → `0.2.0`)
2. รัน (ระบุที่ที่จะวางไฟล์บน server)
   ```powershell
   .\scripts\release.ps1 -BaseUrl https://files.example.internal/docask -Notes "- ค้นเร็วขึ้น`n- แก้ PDF ไทย"
   ```
   สคริปต์จะ build MSI + NSIS ที่เซ็นแล้ว และสร้างโฟลเดอร์ `release\<version>\` ที่มีตัวติดตั้งทั้งสอง + `latest.json`
   (ตัวติดตั้งที่ build ด้วยสคริปต์นี้จะตั้ง URL อัปเดตเป็น `<BaseUrl>/latest.json` ไว้ให้)
3. อัปโหลดทุกไฟล์ใน `release\<version>\` ขึ้น `<BaseUrl>/` (ทับ `latest.json` เดิม)

เครื่องที่ติดตั้งแบบ NSIS จะอัปเดตด้วย NSIS, เครื่องที่ติดตั้งแบบ MSI จะอัปเดตด้วย MSI (ระบุแยกใน `latest.json`)

> `npm run tauri build` ธรรมดาต้องมี env `TAURI_SIGNING_PRIVATE_KEY` ด้วย เพราะเปิด `createUpdaterArtifacts` ไว้ —
> ถ้าจะ build โดยไม่เซ็น ให้ใช้ `npm run tauri build -- --config '{"bundle":{"createUpdaterArtifacts":false}}'`
