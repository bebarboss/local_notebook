# DocAsk — Prompt สำหรับสั่ง AI สร้างแอป (Tauri v2)

> วิธีใช้: วางไฟล์นี้ไว้ที่ root ของโปรเจกต์ (เช่น `SPEC.md`) แล้วสั่ง AI ว่า
> "อ่าน SPEC.md แล้วทำตาม 'ลำดับการทำ' ทีละขั้น ให้รันได้จริงทุกขั้นก่อนไปขั้นต่อไป"
> แนบภาพหน้าจอจาก canvas ดีไซน์ (DocAsk App Screens) ไปด้วยถ้าเครื่องมือรองรับรูปภาพ

---

สร้างแอป desktop ด้วย **Tauri v2** (Rust backend + React/TypeScript/Vite frontend) สำหรับ **Windows x64**
ชื่อแอป: **DocAsk** — ถามตอบจากเอกสารในเครื่อง (RAG) โดยใช้ LLM ของบริษัทผ่าน Gosoft Agent Gateway

## 1. เป้าหมาย

1. ผู้ใช้จัดเอกสารเป็นกลุ่มเรียกว่า **Service** (เช่น HR, POS, จัดซื้อ) แต่ละ service มี index ของตัวเอง
2. แอปแปลงเอกสารเป็นข้อความ ตัดเป็นท่อน ทำ embedding และเก็บ vector **ในเครื่องเท่านั้น**
3. ผู้ใช้เริ่มแชตใหม่ → เลือก service → พิมพ์คำถาม → ค้น vector ใน service นั้น → ส่งคำถาม + ท่อนที่เกี่ยวข้องไป LLM ของบริษัท → แสดงคำตอบแบบ streaming พร้อมแหล่งอ้างอิง
4. แชตถูกบันทึกในเครื่อง เปิดกลับมาคุยต่อได้

## 2. ข้อจำกัดสำคัญ (ห้ามละเมิด)

- เอกสารต้นฉบับและ embedding อยู่ในเครื่องเท่านั้น ห้ามส่งไปบริการภายนอกใดๆ
  ยกเว้นท่อนที่ค้นเจอและประวัติแชตที่ส่งไป Gateway บริษัทตอนถามเท่านั้น
- เครื่องไม่มี GPU: embedding รันบน CPU ผ่าน ONNX Runtime
- ห้ามใช้ Python, Node.js runtime หรือ Electron ในแอปที่ส่งมอบ — ต้องเป็น Tauri + ตัวติดตั้ง MSI/NSIS
- API key ห้ามอยู่ใน frontend, ห้ามเขียนลงไฟล์, ห้ามอยู่ใน log, error, crash report หรือ clipboard
  เก็บใน Windows Credential Manager ผ่าน crate `keyring` เท่านั้น ฝั่ง Rust เป็นคนแนบ key
- เครือข่ายบริษัทอาจใช้ proxy และ CA ภายใน: `reqwest` ต้องอ่าน system proxy
  และใช้ Windows certificate store (`rustls-native-certs` หรือ `native-tls`)
- Gateway เข้าถึงได้เฉพาะตอนต่อ VPN บริษัท แอปต้องบอกผู้ใช้ชัดเจนเมื่อต่อไม่ได้
- ห้ามฝัง API key ตัวอย่างหรือของจริงไว้ในโค้ด, test, README หรือ config ใน repo

## 3. Gateway และ Virtual Key (ทำตามคู่มือ "Gosoft Agent Gateway • VS Code User Guide")

ผู้ใช้คุ้นกับขั้นตอนในคู่มือ VS Code อยู่แล้ว ให้ใช้ชื่อเรียก ลำดับ และค่าเดียวกับคู่มือ

### 3.1 ค่าของ Provider
- Group Name / Endpoint Display Name: `Gosoft Agent Gateway`
- Gateway URL: `https://agent-gateway.gosoft.co.th` (base API: `/v1`, แก้ได้ มีปุ่ม "คืนค่าตามคู่มือ")
- API Type: `Chat Completions` (ล็อกไว้)
- OpenAI-compatible (LiteLLM): `POST /v1/chat/completions` รองรับ `stream: true` (SSE), `GET /v1/models`
- Header: `Authorization: Bearer <virtual key>`

### 3.2 โมเดล 6 ตัว (ค่าตามคู่มือ)
| field | ค่า |
|---|---|
| id / name | `godev1`, `godev2`, `godev3`, `gothink1`, `gothink2`, `gothink3` |
| url | `https://agent-gateway.gosoft.co.th/v1/chat/completions` |
| toolCalling | `true` |
| vision | `false` |
| thinking | `true` |
| contextWindow | `1000000` |
| maxOutputTokens | `64000` |
| supportsReasoningEffort | `["low","medium","high"]` |
| reasoningEffortFormat | `"chat-completions"` |

- ส่ง reasoning effort เป็นฟิลด์ `"reasoning_effort"` ใน body
- ใช้ maxOutputTokens เป็น `max_tokens` สูงสุด และเตือนใน UI ว่าค่าเหล่านี้มาจากคู่มือ ความสามารถจริงของ backend อาจต่ำกว่า
- เก็บ config โมเดลที่ `%APPDATA%\DocAsk\models.json` (ไม่มี key ในไฟล์นี้)
- ปุ่ม "นำเข้าจาก VS Code": เลือกไฟล์ `chatLanguageModels.json` แล้วอ่านรายการ models มาใช้
  **ห้ามอ่านฟิลด์ apiKey** (เป็น secret reference เช่น `${input:chat.lm.secret.xxxx}` ซึ่งอ่าน key จริงไม่ได้)

### 3.3 ข้อความ error (ใช้ทั้งตอนทดสอบและใช้งานจริง)
- DNS ไม่พบ / timeout / connection refused → "ต่อ Gateway ไม่ได้ กรุณาต่อ VPN บริษัทแล้วลองใหม่"
- 401 → "Virtual Key หมดอายุ ผิด หรือถูก revoke — กด Update API Key"
- 403 → "ไม่มีสิทธิ์ใช้โมเดลนี้ ตรวจสิทธิ์ Model/Policy และชนิดของ Key กับ Admin"
- 404 → "Gateway URL หรือชื่อโมเดล (Public Model ID) ไม่ถูกต้อง"
- 429 → "เกินโควตา RPM, TPM หรือ budget — รอสักครู่หรือติดต่อ Admin"
- อื่นๆ → แสดง HTTP status และข้อความจาก server โดยตัด key (`sk-...`) ออกทุกครั้ง

## 4. Embedding (ในเครื่อง)

- ใช้ crate `fastembed` (fastembed-rs) ถ้าใช้ไม่ได้ให้ใช้ `ort` + `tokenizers` โดยตรง
- โมเดลเริ่มต้น: `multilingual-e5-small` (quantized int8) เปลี่ยนเป็น `multilingual-e5-base` หรือ `bge-m3` ได้ใน Settings
- โมเดล e5 ต้องเติม prefix `"passage: "` ตอน index และ `"query: "` ตอนค้น
- ไม่แนบโมเดลในตัวติดตั้ง: ตอนเปิดครั้งแรกให้เลือก
  (ก) ดาวน์โหลดจาก Hugging Face พร้อมแถบความคืบหน้า หรือ
  (ข) เลือกโฟลเดอร์โมเดลที่ดาวน์โหลดไว้แล้ว (กรณีเครือข่ายบริษัทบล็อก huggingface.co)
- เก็บโมเดลที่ `%APPDATA%\DocAsk\models\`
- ทำ embedding แบบ batch ใน background thread ไม่ให้ UI ค้าง

## 5. การแปลงเอกสาร

- รองรับ `.pdf`, `.docx`, `.txt`, `.md` ออกแบบเป็น trait `Extractor` เพื่อเพิ่มชนิดไฟล์ภายหลัง
- PDF: `pdf-extract` หรือ `lopdf` ถ้าภาษาไทยออกมาเพี้ยนให้ใช้ `pdfium-render`
  ถ้าได้ข้อความว่าง (PDF สแกน) ให้ข้ามไฟล์และแสดงสถานะ "ข้าม · PDF สแกน ไม่มีข้อความ" ไม่ต้องทำ OCR
- DOCX: แตก zip แล้วอ่าน `word/document.xml` ด้วย `quick-xml`
- ภาษาไทยไม่มีช่องว่างระหว่างคำ: ตัดท่อนตามจำนวนตัวอักษร (~1000 ตัว, overlap ~150)
  พยายามตัดที่ขึ้นบรรทัด/ย่อหน้า/ช่องว่างก่อน และห้ามตัดกลางสระหรือวรรณยุกต์ (ใช้ `unicode-segmentation` ตัดตาม grapheme cluster)
- เก็บ metadata ต่อท่อน: path, ชื่อไฟล์, เลขหน้า (ถ้ามี), ลำดับท่อน
- Index แบบ incremental: เก็บ hash + mtime ทำใหม่เฉพาะไฟล์ใหม่/แก้ไข ลบท่อนของไฟล์ที่ถูกลบ
- ติดตามโฟลเดอร์ของ service (file watcher, `notify` crate) แล้ว index ไฟล์ใหม่อัตโนมัติ
- งาน index ส่ง progress event ไป frontend และกดยกเลิกได้

## 6. Service (แยกกลุ่มเอกสาร)

- สร้างได้หลาย service แต่ละตัวมี: ชื่อ, รหัสย่อ 2–3 ตัวอักษร, สี, โฟลเดอร์ต้นทาง (หลายโฟลเดอร์ได้),
  โมเดลเริ่มต้น, reasoning effort, top-k, system prompt เพิ่มเติม (ไม่บังคับ)
- index แยกไฟล์ต่อ service: `%APPDATA%\DocAsk\services\<service_id>\index.db`
  เพื่อลบ ส่งออก หรือ index ใหม่ทีละ service โดยไม่กระทบตัวอื่น
- แชตผูกกับ service เดียว ค้นเฉพาะ index ของ service นั้น
  มีตัวเลือก "ค้นทุก service" ในช่องพิมพ์ (ปิดเป็นค่าเริ่มต้น) และระบุ service ในแหล่งอ้างอิงเสมอ
- ไฟล์เดียวกันอยู่ได้หลาย service (เก็บ embedding แยกกัน)
- ลบ service: ถามยืนยัน และถามว่าจะลบแชตของ service นั้นด้วยหรือไม่

## 7. Vector store

- SQLite ผ่าน `rusqlite` (bundled) ต่อ service
- ตาราง `documents` (id, path, hash, mtime, status, chunk_count) และ `chunks` (id, doc_id, page, ord, text, embedding BLOB float32)
- normalize vector ก่อนเก็บ ค้นด้วย dot product แบบ brute force ในหน่วยความจำ (พอสำหรับหลักหมื่นท่อน)
  ออกแบบเป็น trait `VectorStore` เพื่อเปลี่ยนเป็น `sqlite-vec` ภายหลัง
- ค้น top-k (ค่าของ service) และตัดท่อนที่คะแนนต่ำกว่า threshold

## 8. ประวัติแชต

- SQLite ที่ `%APPDATA%\DocAsk\chats.db`:
  `chats` (id, service_id, title, created_at, updated_at) และ
  `messages` (chat_id, role, content, model, reasoning_effort, sources JSON, created_at)
- แถบซ้ายแสดงแชตล่าสุดของ service ที่เลือก เรียงตาม updated_at ค้นหาจากชื่อได้
- เปิดแชตเดิม: โหลดจาก SQLite แสดงทันที ไม่เรียก Gateway
- ถามต่อ: ส่ง N รอบล่าสุด (ค่าเริ่มต้น 6, ตั้งค่าได้) ไปกับคำถามใหม่
  ส่งเฉพาะคำถาม/คำตอบ ไม่ส่งท่อนเอกสารของรอบเก่า และค้น RAG ใหม่จากคำถามปัจจุบันทุกครั้ง
- ตั้งชื่อแชตจาก 40 ตัวอักษรแรกของคำถามแรก เปลี่ยนชื่อและลบได้
- ตั้งค่าลบแชตอัตโนมัติ: ไม่ลบ / 30 / 90 วัน

## 9. Prompt ที่ส่งให้ LLM

- System: ตอบเป็นภาษาเดียวกับคำถาม ใช้เฉพาะข้อมูลจากเอกสารที่ให้ ถ้าเอกสารไม่มีคำตอบให้บอกตรงๆ ว่าไม่พบ
  อ้างอิงแหล่งด้วย `[1]`, `[2]` ตามหมายเลขท่อน (+ system prompt เพิ่มเติมของ service ถ้ามี)
- User: คำถาม + ท่อนเอกสารที่มีหมายเลข `[n]` พร้อมชื่อไฟล์/หน้า
- Frontend แปลง `[n]` ในคำตอบเป็นปุ่มเล็กที่กดแล้วเห็นข้อความท่อนต้นฉบับ

---

## 10. GUI (ตามดีไซน์บน canvas "DocAsk App Screens")

### 10.1 แนวทางรวม
- โครงหน้าแบบแอปแชตสมัยใหม่ (แนว Claude Desktop): แถบซ้ายสำหรับนำทาง, แชตตรงกลางกว้างไม่เกิน 720px, แผงขวาสำหรับเอกสาร
- ฟอนต์: **IBM Plex Sans Thai** (400/500/600/700) สำหรับทุกข้อความ, **IBM Plex Mono** สำหรับโค้ด, URL, ชื่อโมเดล, key
  ฝังไฟล์ฟอนต์ไว้ในแอป (ไม่โหลดจาก Google Fonts ตอนรัน เพราะเครื่องอาจออฟไลน์)
- ข้อความ UI ภาษาไทยทั้งหมด
- มุมโค้ง: ปุ่ม/ช่องกรอก 10–12px, การ์ด 12–16px, กล่องพิมพ์ 20px, ชิป 999px
- ทุกปุ่มสูงอย่างน้อย 36px (ปุ่มหลัก 44px), ใช้ `<button>`, `<a>`, `<input>` + `<label>` จริง, ปุ่มไอคอนต้องมี `aria-label`
- ไอคอน: เส้นแบบ stroke 1.8px (เช่น lucide) ห้ามใช้ emoji เป็นไอคอน
- ใช้ CSS variables ทั้งหมด รองรับ **ธีมสว่าง / มืด / ตามระบบ** (`prefers-color-scheme`) สลับได้ใน Settings

### 10.2 Design tokens
| token | Light | Dark | ใช้กับ |
|---|---|---|---|
| `--bg` | `#F5F6F8` | `#14161A` | พื้นหลังแอป |
| `--sidebar` | `#EDEFF3` | `#1B1E23` | แถบซ้าย, กล่องข้อมูลรอง |
| `--surface` | `#FFFFFF` | `#1F2228` | การ์ด, ช่องกรอก, กล่องพิมพ์ |
| `--border` | `#E3E6EB` | `#2E323A` | เส้นขอบหลัก |
| `--border-soft` | `#EEF0F3` | `#272A31` | เส้นคั่นแถว |
| `--text` | `#16181D` | `#ECEEF2` | ข้อความหลัก |
| `--text-2` | `#3B414C` | `#C9CDD5` | ข้อความรอง |
| `--muted` | `#5B6270` | `#9AA1AD` | คำอธิบาย, เวลา |
| `--accent` | `#1F4FD1` | `#3B66DB` | ปุ่มหลัก, แถบความคืบหน้า |
| `--accent-text` | `#1F4FD1` | `#8EACF8` | ลิงก์, ป้ายขั้นตอน |
| `--accent-soft` | `#EEF2FD` | `#1E2A45` | รายการที่เลือก, ชิปอ้างอิง |
| `--accent-ink` | `#173B9E` | `#B9CCFB` | ข้อความบน accent-soft |
| `--on-accent` | `#FFFFFF` | `#FFFFFF` | ข้อความบนปุ่มหลัก |
| `--user-bubble` | `#E9ECF2` | `#272B33` | กล่องข้อความผู้ใช้ |
| `--avatar` | `#D7DCE5` | `#353A44` | วงกลมตัวย่อชื่อ |
| `--ok-text` / `--ok-bg` / `--ok-dot` | `#12663C` / `#E7F5EE` / `#1A7F4B` | `#6FD39B` / `#16301F` / `#3FBF77` | สำเร็จ, Gateway พร้อม |
| `--err-text` / `--err-bg` / `--err-border` | `#9B1C12` / `#FDECEA` / `#F1C4BF` | `#F4A39A` / `#3A1C19` / `#5C2A24` | ผิดพลาด, ลบ |
| `--warn-text` / `--warn-bg` / `--warn-border` | `#7A4A00` / `#FFF7E8` / `#F2D49B` | `#F2C878` / `#33270F` / `#5A4318` | คำเตือน, ไฟล์ถูกข้าม |
| `--track` | `#E3E6EB` | `#2E323A` | พื้นแถบความคืบหน้า |
| `--disabled` | `#B7BDC7` | `#4A505B` | ปุ่มที่กดไม่ได้ |
| `--shadow` | `0 2px 10px rgba(20,30,50,.06)` | `0 2px 10px rgba(0,0,0,.35)` | กล่องพิมพ์, การ์ด setup |

สีประจำ service ตัวอย่าง: HR `#1F4FD1`, POS `#B45309`, จัดซื้อ `#0F766E`, ประชุม `#7C3AED` (ผู้ใช้เลือกได้ ข้อความบนสีต้องเป็นสีขาวและคอนทราสต์ ≥ 4.5:1)

### 10.3 หน้าจอ

**A. หน้าแชต (หลัก)** — 3 คอลัมน์ (แคบลงแล้วซ้อนกันได้)
- แถบซ้าย (กว้าง 240–260px, `--sidebar`):
  โลโก้สี่เหลี่ยมมน "D" + "DocAsk" · ปุ่ม "แชตใหม่" (วงกลม accent + ไอคอน +) ·
  หัวข้อ "Service" + ลิงก์ "จัดการ" · รายการ service (สี่เหลี่ยมสี 10px + ชื่อ + จำนวนเอกสาร, ตัวที่เลือกพื้น `--accent-soft`) ·
  "แชตล่าสุดใน <service>" (แชตที่เปิดอยู่พื้น `--user-bubble`) ·
  ล่างสุด: วงกลมตัวย่อชื่อผู้ใช้ + ชื่อผู้ใช้ + สถานะ "● Gateway พร้อม · VPN" (หรือแดง "ต่อ Gateway ไม่ได้")
- ตรงกลาง:
  แถบบน: ชิป service (สี + ชื่อ) / ชื่อแชต · ปุ่ม "เอกสาร N" (ซ่อน/แสดงแผงขวา)
  ข้อความผู้ใช้: กล่องมน 16px พื้น `--user-bubble` มีวงกลมตัวย่อชื่อด้านซ้าย
  คำตอบ AI: ข้อความล้วนไม่มีกรอบ ตัวอักษร 15px line-height 1.85, ตัวหนาสำหรับคำสำคัญ, `[n]` เป็นชิปเล็ก `--accent-soft`
  ใต้คำตอบ: ปุ่มไอคอน คัดลอก / ถามใหม่ · "godev1 · medium · 2.4 วิ" · ชิปแหล่งอ้างอิง "1 · ไฟล์.pdf · น.4"
  กล่องพิมพ์ (ล่าง, มน 20px, เงา): textarea "ถามอะไรก็ได้เกี่ยวกับเอกสารของคุณ" ·
  แถวล่าง: ปุ่ม + (เพิ่มเอกสาร) · ชิป "ค้นใน: HR เท่านั้น" (กดสลับเป็นทุก service) · ตัวเลือก "godev1 · medium ▾" · ปุ่มส่งสี่เหลี่ยมมน accent ไอคอนลูกศรขึ้น
  ใต้กล่อง: "ค้นเฉพาะเอกสารใน service HR แล้วส่ง 5 ท่อนที่เกี่ยวข้องไปยัง Gateway บริษัท"
- แผงขวา (260–300px): "เอกสารใน <service>" + ปุ่ม "+ เพิ่ม" · แถบพื้นที่ใช้ + "96 ท่อน · 6.1 MB" ·
  การ์ดเอกสาร (ชื่อไฟล์, ชนิด · จำนวนท่อน / แถบ "กำลังทำ index 64%" / การ์ดสีเตือน "ข้าม · PDF สแกน") ·
  กล่อง "ค่าของ service นี้" (โมเดล, top-k, ค้นข้าม service) · ล่างสุด "เอกสารและ vector ของแต่ละ service เก็บแยกกันในเครื่องนี้"
- กดชิป `[n]` → แสดงข้อความท่อนต้นฉบับ (ชื่อไฟล์, หน้า, คะแนน) + ปุ่ม "เปิดไฟล์ต้นฉบับ"

**B. แชตใหม่ — เลือก service ก่อน**
- แถบซ้ายเหมือนหน้า A ("แชตใหม่" ถูกไฮไลต์)
- กลางจอ: หัวข้อ "สวัสดี <ชื่อผู้ใช้>" (28px ตัวหนา) + "เลือก service ที่จะใช้ค้นเอกสารก่อนเริ่มแชต"
- radiogroup การ์ด service 2 คอลัมน์: สี่เหลี่ยมสีพร้อมรหัสย่อ · ชื่อ · "3 เอกสาร · godev1 · medium" · วงกลม radio
  การ์ดที่เลือก: ขอบ 2px `--accent` พื้น `--accent-soft`
- กล่องพิมพ์ด้านล่าง: **ก่อนเลือก** จาง (opacity .55) พิมพ์และส่งไม่ได้ placeholder "เลือก service ด้านบนก่อนเริ่มพิมพ์" ปุ่มส่งสี `--disabled`
  **หลังเลือก** ใช้งานได้ แสดง "ค้นใน: <service>" และโมเดลเริ่มต้นของ service
- เปลี่ยน service ได้จนถึงก่อนส่งข้อความแรก หลังจากนั้นแชตผูกกับ service เดิมตลอด
- จำ service ล่าสุดไว้เป็นตัวเลือกแนะนำ แต่ไม่เลือกให้อัตโนมัติ

**C. จัดการ Service**
- ลิงก์ "← กลับไปแชต" · หัวข้อ "Service" · คำอธิบาย · ปุ่มหลัก "+ สร้าง service ใหม่"
- grid การ์ด (auto-fill, กว้างขั้นต่ำ 300px): รหัสย่อบนสี่เหลี่ยมสี · ชื่อ · สถานะ ("กำลังใช้งาน", "อัปเดต 2 ชม. ที่แล้ว", "ข้าม 1 ไฟล์")
  · ตัวเลข 3 ช่อง: เอกสาร / ท่อน / ขนาด · โมเดล, effort, top-k, โฟลเดอร์ · ปุ่ม "ตั้งค่า" และ "+ เอกสาร"
  · service ที่ใช้งานอยู่ขอบ 2px accent
- การ์ดสุดท้ายเป็นปุ่มเส้นประ "สร้าง service ใหม่ — เลือกโฟลเดอร์ ตั้งชื่อ และเลือกโมเดลเริ่มต้น"
- กล่องข้อมูลล่าง: index แยกไฟล์ต่อ service, ลบ/ส่งออกทีละ service ได้, ไม่ค้นข้าม service ยกเว้นเปิด "ค้นทุก service"
- หน้าต่างสร้าง/แก้ไข service: ชื่อ, รหัสย่อ, สี, โฟลเดอร์ (เพิ่ม/ลบ), โมเดล, effort, top-k, system prompt, ปุ่มลบ service (ยืนยัน)

**D. Setup Wizard (เปิดอัตโนมัติครั้งแรก, เปิดซ้ำได้จาก Settings)**
- พื้น `--bg` กลางจอมีโลโก้ DocAsk และการ์ด `--surface` มน 20px กว้าง 560–640px มีเงา
- ส่วนหัวการ์ด: "ขั้นที่ N จาก 4" (สี accent-text) · หัวข้อ 24px ตัวหนา · คำอธิบาย · แถบขั้นตอน 4 ช่อง (ผ่านแล้วสี accent)
- ปุ่มล่างขวา: "ย้อนกลับ" (ขอบ) และปุ่มหลัก "ดำเนินการต่อ" / "เริ่มใช้งาน"
- ขั้นที่ 1 Provider: ตารางค่า Group/Display Name, Gateway URL, API Type ตามข้อ 3.1 (แก้ URL ได้ + "คืนค่าตามคู่มือ")
- ขั้นที่ 2 Virtual Key: แสดง Provider/API Type แบบตาราง 2 แถว · `<label>` "API Key" · ช่อง password (ฟอนต์ mono, ขอบ accent + วง focus `--ring`) + ปุ่ม "แสดง"
  · ตรวจรูปแบบ (ขึ้นต้น `sk-`, ไม่มีช่องว่าง) แสดง "✓ รูปแบบถูกต้อง" สี ok ·
  กล่องเตือนสี warn: "**ความปลอดภัย:** ใช้ Virtual Key รายบุคคลเท่านั้น ห้ามใช้ LiteLLM Master Key และห้ามส่ง Screenshot ที่เห็น Key"
  · "Key จะถูกเก็บใน Windows Credential Manager ไม่เขียนลงไฟล์หรือ log"
  · บันทึกด้วย service name `DocAsk/Gosoft Agent Gateway` หลังบันทึกแสดงแค่ `sk-••••••••` + 4 ตัวท้าย
- ขั้นที่ 3 โมเดล: รายการ 6 โมเดลตามข้อ 3.2 (เปิด/ปิดได้) + ปุ่ม "นำเข้าจาก VS Code"
- ขั้นที่ 4 ทดสอบ: ส่ง `Reply exactly: GOSOFT LITELLM OK` ทีละโมเดล
  ตารางแถวละโมเดล: วงกลม ✓ (ok) หรือ ✕ (err) · ชื่อโมเดล (mono) · ข้อความที่ได้หรือคำแนะนำตามข้อ 3.3 · เวลาตอบ
  แถวที่ล้มเหลวพื้น `--err-bg` · ผ่านเมื่อคำตอบมี "GOSOFT LITELLM OK" (ไม่สนตัวพิมพ์/ช่องว่างหัวท้าย)
  หมายเหตุ: "ผลสำเร็จไม่ได้ยืนยันว่า backend ใช้ค่า reasoning effort จริง · โมเดลที่ไม่ผ่านจะถูกซ่อนไว้ก่อน"
  ปุ่ม "ทดสอบอีกครั้ง" และ "เริ่มใช้งาน" (กดได้แม้บางโมเดลล้มเหลว)

**E. Settings**
- หัวข้อ "ตั้งค่า" 26px · เมนูซ้าย (Gateway และ Key, โมเดล, Embedding และ Index, การค้นหา, ประวัติแชต, ธีม, วิธีแก้ปัญหา, เกี่ยวกับ)
  รายการที่เลือกพื้น `--accent-soft`
- หมวด Gateway: การ์ดรายการแถวละหนึ่งค่า (ซ้ายชื่อ + คำอธิบาย, ขวาปุ่มหรือสถานะ):
  Gateway URL + "● เชื่อมต่อได้ · 5/6 โมเดล" · API Key `sk-•••• x7Kp · Windows Credential Manager` + ปุ่มหลัก "Update API Key" ·
  ทดสอบการเชื่อมต่อ (เวลาทดสอบล่าสุด, ผล) + ปุ่ม "ทดสอบ" · ธีม segmented control สว่าง/มืด/ตามระบบ ·
  ลบ Provider + ปุ่มขอบแดง "Remove Provider" (ยืนยัน, ลบ key จาก Credential Manager และ config)
- "Update API Key": ใส่ key ใหม่แล้วทดสอบอีกครั้งโดยไม่ต้องตั้งค่า 6 โมเดลใหม่
- หมวดอื่น: โมเดล (เปิด/ปิด, ค่าเริ่มต้น) · Embedding (เลือกโมเดล, ดาวน์โหลด/เลือกโฟลเดอร์, index ใหม่ทั้งหมด) ·
  การค้นหา (top-k, threshold, ขนาดท่อน) · ประวัติแชต (จำนวนรอบที่ส่ง, ลบอัตโนมัติ, ลบทั้งหมด) ·
  เกี่ยวกับ (เวอร์ชัน, ปุ่ม "ลบข้อมูลทั้งหมด")

**F. วิธีแก้ปัญหา** (ใน Settings และเมนู Help)
- ตาราง 2 คอลัมน์ อาการ | ตรวจสอบ: ต่อ Gateway ไม่ได้ → ต่อ VPN · 401 · 403 · 404 · 429 (ตามข้อ 3.3) · ไม่เห็นโมเดล → ทดสอบโมเดลอีกครั้งและตรวจ config

### 10.4 สถานะที่ต้องมีทุกหน้า
- โหลด (skeleton), ว่าง (ยังไม่มี service/เอกสาร/แชต พร้อมปุ่มให้ทำขั้นแรก), ผิดพลาด (ข้อความตามข้อ 3.3 + ปุ่มลองใหม่)
- Gateway ต่อไม่ได้: แถบแจ้งเตือนด้านบน + ปุ่มส่งกดไม่ได้ แต่ยังค้นและดูเอกสาร/แชตเก่าได้
- ระหว่าง streaming: ปุ่มส่งเปลี่ยนเป็นปุ่ม "หยุด"

---

## 11. ความปลอดภัยของ Tauri

- เปิด capabilities เท่าที่จำเป็น: dialog (เลือกไฟล์/โฟลเดอร์), shell open (เปิดไฟล์ต้นฉบับ) เท่านั้น
- CSP ให้ frontend เรียกได้เฉพาะ IPC ของ Tauri ห้ามเรียก HTTP ออกจาก frontend
- เรียก Gateway, อ่านไฟล์, จัดการ key ทั้งหมดอยู่ใน Rust commands
- frontend รู้แค่สถานะ "มี key แล้ว/ยังไม่มี" และ 4 ตัวท้าย
- ฟังก์ชัน `redact()` ลบ `sk-...` ออกจากทุกข้อความ error และ log พร้อม unit test

## 12. โครงสร้างโค้ด

```
src-tauri/src/
  main.rs, commands.rs, config.rs, secrets.rs, errors.rs, redact.rs
  ingest/{mod.rs, pdf.rs, docx.rs, text.rs, chunker.rs, watcher.rs}
  embed.rs, store.rs, search.rs, services.rs, chats.rs
  llm.rs (chat completions + SSE streaming), gateway_test.rs
src/
  App.tsx, theme.css (tokens ข้อ 10.2), i18n/th.ts
  pages/{Chat, NewChat, Services, Setup, Settings}
  components/{Sidebar, MessageList, Composer, CitationChip, SourcePanel, DocPanel,
              ServiceCard, ModelPicker, StatusPill, ProgressBar, TroubleshootTable}
  lib/api.ts (wrapper ของ invoke/listen + mock mode)
```

## 13. การพัฒนาและ preview

- `npm run tauri dev`: แอปจริง + hot reload
- `npm run dev`: เปิดหน้าจอในเบราว์เซอร์ที่ `http://localhost:5173`
- **Mock mode**: ถ้า frontend ไม่ได้รันใน Tauri หรือตั้ง `DOCASK_MOCK=1` ให้ `lib/api.ts` ใช้ข้อมูลตัวอย่างแทนทุก command:
  4 services, เอกสารตัวอย่าง (รวมไฟล์กำลัง index และไฟล์ถูกข้าม), คำตอบ streaming สมมติพร้อม `[1][2]`,
  ผลทดสอบ 6 โมเดล (มี 1 ตัว 403) · ห้ามเรียกเครือข่ายใดๆ และแสดงป้าย "MOCK MODE" มุมจอ
- README อธิบายการติดตั้ง Node, Rust, Visual Studio Build Tools (C++) บน Windows และคำสั่ง dev/build

## 14. การแพ็กและติดตั้ง

- `npm run tauri build` ต้องได้ทั้ง **MSI** (WiX) และ **NSIS setup.exe** สำหรับ Windows x64
- NSIS: `installMode: "currentUser"` (ไม่ต้องใช้ admin) มีภาษาไทยในตัวติดตั้ง
- MSI: รองรับ silent install `msiexec /i DocAsk.msi /qn` สำหรับ IT แจกจ่าย
- WebView2: `webviewInstallMode` = `"skip"` หรือ `"downloadBootstrapper"` (ตั้งค่าได้) ไม่ฝัง runtime
- ไม่แนบโมเดล embedding ในตัวติดตั้ง (มี build flag แยกสำหรับรุ่นที่แนบโมเดล)
- เตรียม config code signing (`certificateThumbprint`, `timestampUrl`) ใน `tauri.conf.json` เป็นค่าว่างให้ IT ใส่ ห้ามใส่ใบรับรองหรือรหัสผ่านใน repo
- ข้อมูลผู้ใช้อยู่ที่ `%APPDATA%\DocAsk` ไม่ลบตอนถอนการติดตั้ง (ลบได้จากปุ่ม "ลบข้อมูลทั้งหมด")
- เลขเวอร์ชันจาก `package.json`/`tauri.conf.json` แสดงในหน้า Settings → เกี่ยวกับ

## 15. ลำดับการทำ (ทำทีละขั้น ให้รันได้จริงก่อนไปขั้นต่อไป)

1. โครง Tauri v2 + theme tokens light/dark + mock mode + โครงหน้าทุกหน้าด้วยข้อมูล mock
2. Setup Wizard 4 ขั้น (Provider, Virtual Key ใน Credential Manager, 6 โมเดล + นำเข้าจาก VS Code, ทดสอบ "GOSOFT LITELLM OK") + Update API Key + ตารางแก้ปัญหา
3. แชตกับ Gateway แบบ streaming (ยังไม่มี RAG) + บันทึกประวัติแชต
4. ตัวแปลง txt/md/docx/pdf + chunker ภาษาไทย พร้อม unit test
5. Embedding ในเครื่อง + SQLite store ต่อ service + ค้น
6. Service: สร้าง/แก้ไข/ลบ, หน้าแชตใหม่ที่ต้องเลือก service, ผูกแชตกับ service
7. รวมเป็น RAG + แหล่งอ้างอิง + แผงเอกสาร
8. Incremental index, file watcher, ยกเลิกงาน, จัดการ error ทุกกรณี
9. Build MSI + NSIS

## 16. สิ่งที่ต้องส่งมอบ

- โค้ดครบที่ build ได้ด้วย `npm install && npm run tauri build`
- README ภาษาไทย: build, ติดตั้ง, ใส่ key, ดาวน์โหลดโมเดลแบบ offline, mock mode, แก้ปัญหาที่พบบ่อย
- Unit test: chunker (ไทย, อังกฤษ, ผสม), vector search, redact, การแยก index ต่อ service
- ไม่มี API key ใดๆ ในโค้ด, test, README หรือ config
