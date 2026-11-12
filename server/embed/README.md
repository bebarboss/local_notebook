# DocAsk embedding server

เซิร์ฟเวอร์ทำ embedding ให้ DocAsk รันใน Docker บนเครื่องที่บ้าน (เขียนไว้สำหรับ Mac mini Apple Silicon และใช้กับเครื่อง Linux หรือ Windows ได้ด้วย) แล้วต่อจากที่ทำงานผ่าน Tailscale

- ใช้โมเดล `BAAI/bge-m3` (1024 มิติ, รองรับภาษาไทย) แบบ fp32 เต็มความแม่น ไม่ใช่ int8 แบบที่แอปใช้ในเครื่อง
- API เป็นแบบเดียวกับ OpenAI `POST /v1/embeddings` และต้องส่ง `Authorization: Bearer <token>`
- ไม่เก็บและไม่ log ข้อความที่ส่งมา log แค่จำนวนข้อความกับเวลาที่ใช้

## สิ่งที่ต้องมีบน Mac mini

- **Docker Desktop:** ตั้งค่า › General › เปิด *Start Docker Desktop when you sign in* และตั้งค่า › Resources › Memory **อย่างน้อย 6 GB**
  - ใช้ OrbStack แทนได้ กิน RAM น้อยกว่า
- **Tailscale:** ลงทั้งบน Mac mini และเครื่อง Windows ที่ใช้ DocAsk แล้ว login บัญชีเดียวกัน
  - ที่ <https://login.tailscale.com/admin/dns> ให้เปิด **MagicDNS** และ **HTTPS Certificates**
- **ไม่ให้เครื่องหลับ:** ตั้งค่าระบบ › พลังงาน › เปิด *ป้องกันการพักเครื่องอัตโนมัติเมื่อจอดับ*

> Docker บน Mac ใช้ GPU (Metal) ไม่ได้ จึงรันบน CPU ซึ่งชิป M-series ทำ bge-m3 ได้ราว 32 ท่อนใน 3–8 วินาที

## ติดตั้ง

**1. คัดลอกโฟลเดอร์ `server/embed` ไปไว้บน Mac แล้วสร้าง token**

```bash
cd server/embed
cp .env.example .env
echo "EMBED_TOKEN=$(openssl rand -hex 32)" > .env
cat .env          # ← คัดลอก token นี้ไว้ใส่ใน DocAsk
```

**2. build และเปิด**

```bash
docker compose up -d --build
docker compose logs -f
```

ครั้งแรกจะดาวน์โหลดโมเดลราว 2.3 GB เก็บไว้ใน volume `hf-cache` ใช้เวลาหลายนาที จนกว่า log จะขึ้นว่า `loaded BAAI/bge-m3 (1024 dims) on cpu` แล้วค่อยลองเรียก

```bash
curl http://127.0.0.1:8080/health
# {"status":"ok","model":"bge-m3","dim":1024,"device":"cpu"}
```

**3. เปิดให้เครื่องที่ทำงานเข้าถึงผ่าน Tailscale (HTTPS)**

```bash
tailscale serve --bg 8080
tailscale serve status    # ได้ URL เช่น https://mac-mini.tail1234.ts.net
```

container ผูก port ไว้ที่ `127.0.0.1` เท่านั้น เครื่องอื่นใน LAN หรืออินเทอร์เน็ตจึงเข้าตรงไม่ได้ เข้าได้ทาง `tailscale serve` ซึ่งเห็นเฉพาะเครื่องใน tailnet ของคุณ

> ถ้าเรียก `tailscale` ใน Terminal ไม่ได้ (ลงจาก App Store) ให้ใช้ `/Applications/Tailscale.app/Contents/MacOS/Tailscale serve --bg 8080`

**4. ตั้งค่าใน DocAsk (เครื่อง Windows)**

1. เปิด ตั้งค่า › Embedding และ Index › **หรือใช้เซิร์ฟเวอร์ embedding ของตัวเอง**
2. ใส่ URL จากข้อ 3 และ token จากข้อ 1 แล้วกด **บันทึกและทดสอบ**
3. ถ้าขึ้นว่า "ใช้ได้" ให้กด **ใช้เซิร์ฟเวอร์นี้** แอปจะทำ embedding ใหม่ทุก service

token ถูกเก็บใน Windows Credential Manager ไม่ได้เก็บในไฟล์

## ดูแลรักษา

| งาน | คำสั่ง |
|---|---|
| ดูสถานะ | `docker compose ps` (คอลัมน์ STATUS ต้องเป็น `healthy`) |
| ดู log | `docker compose logs --tail 50` |
| อัปเดตโค้ด | คัดลอกไฟล์ใหม่ทับ แล้ว `docker compose up -d --build` |
| เปลี่ยน token | แก้ `.env` → `docker compose up -d` → ใส่ token ใหม่ใน DocAsk |
| หยุด | `docker compose down` (โมเดลยังอยู่ใน volume) |

container มี `restart: unless-stopped` จึงกลับมาทำงานเองหลังรีสตาร์ทเครื่อง ตราบใดที่ Docker Desktop ตั้งให้เปิดตอน login

## ถ้าใช้ไม่ได้

| อาการใน DocAsk | ตรวจ |
|---|---|
| ติดต่อเซิร์ฟเวอร์ embedding ไม่ได้ | Mac หลับหรือปิดอยู่ไหม · `docker compose ps` · Tailscale บน Windows ต่ออยู่ไหม · `tailscale serve status` |
| token ไม่ถูกต้อง | token ใน DocAsk ตรงกับ `EMBED_TOKEN` ใน `.env` ไหม (ถ้าแก้ `.env` ต้อง `docker compose up -d` ใหม่) |
| เซิร์ฟเวอร์ไม่มีโมเดล … | เปลี่ยน `EMBED_MODEL` แล้วหรือเปล่า ให้กดทดสอบและ "ใช้เซิร์ฟเวอร์นี้" ใหม่ |
| container restart วน | RAM ของ Docker ไม่พอ ให้เพิ่มเป็น 6 GB ขึ้นไป |

## ความปลอดภัย

- ห้าม commit ไฟล์ `.env` (อยู่ใน `.gitignore` แล้ว)
- ห้ามเปลี่ยน port เป็น `0.0.0.0:8080` หรือเปิด port ที่ router เพราะข้อความในเอกสารของบริษัทจะวิ่งผ่านเส้นนี้
- ห้ามใช้ `tailscale funnel` เพราะจะเปิดให้ทั้งอินเทอร์เน็ตเข้าถึง ให้ใช้ `serve` เท่านั้น
- DocAsk ยอมใช้ `http://` เฉพาะกับ localhost และที่อยู่ Tailscale (`100.64.0.0/10`, `*.ts.net`) ที่อื่นต้องเป็น `https://`
- เครื่อง Windows ของบริษัทอาจไม่อนุญาตให้ลง Tailscale หรือส่งข้อมูลออกนอกเครือข่าย ให้ตรวจนโยบายบริษัทก่อนใช้

## ตั้งค่าเพิ่มเติม (`.env`)

| ตัวแปร | ค่าเริ่มต้น | ความหมาย |
|---|---|---|
| `EMBED_TOKEN` | (ต้องใส่) | ยาวอย่างน้อย 16 ตัว |
| `EMBED_MODEL` | `BAAI/bge-m3` | โมเดล sentence-transformers จาก Hugging Face (เปลี่ยนแล้ว DocAsk ต้อง index ใหม่) |
| `EMBED_MODEL_REVISION` | `main` | ระบุ commit ของโมเดลเพื่อให้ผลไม่เปลี่ยน |
| `EMBED_QUERY_PREFIX` / `EMBED_PASSAGE_PREFIX` | ว่าง | สำหรับโมเดลตระกูล e5 ให้ใส่ `query: ` / `passage: ` |
| `EMBED_MAX_TOKENS` | `2048` | ตัดข้อความที่ยาวกว่านี้ |
| `EMBED_BATCH` | `16` | จำนวนข้อความต่อรอบ ลดลงถ้า RAM ไม่พอ |

## ทดสอบโค้ด (ไม่ต้องดาวน์โหลดโมเดล)

```bash
pip install fastapi httpx pytest
EMBED_NO_AUTOLOAD=1 python -m pytest -q
```
