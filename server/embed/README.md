# DocAsk embedding server

เซิร์ฟเวอร์ทำ embedding ให้ DocAsk รันใน Docker บนเครื่องที่บ้าน (เขียนไว้สำหรับ Mac mini Apple Silicon และใช้กับเครื่อง Linux หรือ Windows ได้ด้วย) แล้วต่อจากที่ทำงานผ่าน ngrok (HTTPS)

- ใช้โมเดล `BAAI/bge-m3` (1024 มิติ, รองรับภาษาไทย) แบบ fp32 เต็มความแม่น ไม่ใช่ int8 แบบที่แอปใช้ในเครื่อง
- API เป็นแบบเดียวกับ OpenAI `POST /v1/embeddings` และต้องส่ง `Authorization: Bearer <token>`
- ไม่เก็บและไม่ log ข้อความที่ส่งมา log แค่จำนวนข้อความกับเวลาที่ใช้

## สิ่งที่ต้องมีบน Mac mini

- **Docker Desktop:** ตั้งค่า › General › เปิด *Start Docker Desktop when you sign in* และตั้งค่า › Resources › Memory **อย่างน้อย 6 GB**
  - ใช้ OrbStack แทนได้ กิน RAM น้อยกว่า
- **บัญชี ngrok** (สมัครฟรีที่ <https://dashboard.ngrok.com>) ไม่ต้องลงโปรแกรมใดๆ บนเครื่อง Windows
  - **Your Authtoken** (เมนู Getting Started)
- **ไม่ให้เครื่องหลับ:** ตั้งค่าระบบ › พลังงาน › เปิด *ป้องกันการพักเครื่องอัตโนมัติเมื่อจอดับ*

> Docker บน Mac ใช้ GPU (Metal) ไม่ได้ จึงรันบน CPU ซึ่งช้า (M2: ราว 20–40 วินาทีต่อ 32 ท่อน) บน Mac แนะนำ **โหมดเร็ว** ด้านล่าง

## ติดตั้ง

**1. คัดลอกโฟลเดอร์ `server/embed` ไปไว้บน Mac แล้วสร้าง `.env`**

```bash
cd server/embed
cp .env.example .env
sed -i '' "s/^EMBED_TOKEN=.*/EMBED_TOKEN=$(openssl rand -hex 32)/" .env
grep EMBED_TOKEN .env   # ← คัดลอก token นี้ไว้ใส่ใน DocAsk
```

แล้วเปิด `.env` ใส่ค่า ngrok

```ini
NGROK_AUTHTOKEN=<authtoken จาก dashboard>
NGROK_ALLOW_IPS=203.0.113.10/32      # IP สาธารณะของที่ทำงาน (เปิด https://ifconfig.me บนเครื่อง Windows)
```

`NGROK_ALLOW_IPS` ใส่ได้หลายค่า คั่นด้วย `,` ถ้าเว้นว่างไว้ ใครที่รู้ URL ก็ยิงเข้ามาได้ (ยังต้องมี token อยู่ดี)

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

**3. ตรวจ ngrok**

container `docask-ngrok` เปิดพร้อมกันจากข้อ 2 แล้ว

ดู URL ปัจจุบัน

```bash
docker compose logs ngrok | grep -o 'url=https://[^ ]*' | tail -1
curl https://<URL ที่ได้>/health
```

**URL อาจเปลี่ยนทุกครั้งที่ ngrok รีสตาร์ท** (รีบูตเครื่อง, `docker compose up -d`, เน็ตหลุดนาน) ถ้า DocAsk ขึ้นว่าติดต่อเซิร์ฟเวอร์ไม่ได้ ให้รันคำสั่งข้างบนแล้วใส่ URL ใหม่ใน DocAsk ถ้าไม่อยากเปลี่ยนบ่อย ให้ใส่ `NGROK_DOMAIN` เป็น static domain จาก dashboard (เมนู Domains)

port 8080 ผูกไว้ที่ `127.0.0.1` เท่านั้น เครื่องอื่นใน LAN เข้าตรงไม่ได้ ช่องทางเดียวจากข้างนอกคือ ngrok ซึ่งกรอง IP (ถ้าตั้ง `NGROK_ALLOW_IPS`) แล้วจึงส่งต่อให้เซิร์ฟเวอร์ตรวจ token

**4. ตั้งค่าใน DocAsk (เครื่อง Windows)**

1. เปิด ตั้งค่า › Embedding และ Index › **หรือใช้เซิร์ฟเวอร์ embedding ของตัวเอง**
2. ใส่ URL จากข้อ 3 และ token จากข้อ 1 แล้วกด **บันทึกและทดสอบ**
3. ถ้าขึ้นว่า "ใช้ได้" ให้กด **ใช้เซิร์ฟเวอร์นี้** แอปจะทำ embedding ใหม่ทุก service

token ถูกเก็บใน Windows Credential Manager ไม่ได้เก็บในไฟล์

**ฝัง URL + token มากับโปรแกรม (v0.12.1 ขึ้นไป)** — GitHub repo › Settings › Secrets and variables › Actions ›
เพิ่ม secret `DOCASK_EMBED_URL` (เช่น `https://xxxx.ngrok-free.app` — ควรเป็น `NGROK_DOMAIN` แบบ static) และ
`DOCASK_EMBED_TOKEN` (= `EMBED_TOKEN`) แล้วออกเวอร์ชันใหม่ · เครื่องที่ยังไม่ได้ดาวน์โหลด bge-m3 จะใช้เซิร์ฟเวอร์นี้เอง ·
token ที่ฝังมาเปลี่ยนในแอปไม่ได้ เปลี่ยนได้แค่ URL · ไม่อยู่ในโค้ดของ repo แต่**ไฟล์ติดตั้งเป็น public ใครก็แกะ token ออกได้**
→ ตั้ง `NGROK_ALLOW_IPS` ไว้ด้วย คนนอก IP ที่อนุญาตจะเข้าไม่ถึงเลยแม้มี token · เปลี่ยน token: แก้ `.env` +
`docker compose up -d` + แก้ secret แล้วออกเวอร์ชันใหม่

**5. (แนะนำ) โมเดลสำรองในเครื่อง** — ใต้ผลทดสอบกด **ดาวน์โหลด bge-m3 ไว้สำรอง** (~570 MB) ถ้า Mac ปิดหรือเน็ตหลุด
DocAsk จะคำนวณคำถามในเครื่องแทนและยังค้นเอกสารได้ (index อยู่บน Windows อยู่แล้ว) · ไฟล์ใหม่จะรอ index จนเซิร์ฟเวอร์กลับมา

## โหมดเร็ว (Mac Apple Silicon, GPU)

รันเซิร์ฟเวอร์บน macOS ตรงๆ ให้ใช้ GPU (Metal) ได้ เร็วกว่า Docker ราว 6–10 เท่า (M2: ราว 3 วินาทีต่อ 32 ท่อน) และได้ vector เดิมทุกตัว ไม่ต้อง index ใหม่ ngrok ยังรันใน Docker เหมือนเดิม

```bash
./mac-native.sh install      # ครั้งแรกติดตั้ง Python 3.12 + PyTorch ใน ~/.docask-embed และดาวน์โหลดโมเดล ~2.3 GB
./mac-native.sh status       # ต้องเห็น "device":"mps:0"
```

แล้วแก้ `.env` ให้ ngrok ส่งต่อไปที่เซิร์ฟเวอร์ตัวนี้ และปิด container CPU

```ini
COMPOSE_PROFILES=
NGROK_UPSTREAM=host.docker.internal:8081
```

```bash
docker compose up -d ngrok
docker compose --profile cpu rm -sf embed
```

URL ของ ngrok จะเปลี่ยน (ถ้าไม่ได้ตั้ง `NGROK_DOMAIN`) ให้ใส่ URL ใหม่ใน DocAsk ตามข้อ 3–4

- เซิร์ฟเวอร์เริ่มเองตอน login ผ่าน launchd (`~/Library/LaunchAgents/com.docask.embed.plist`) และเปิดใหม่เองถ้าล่ม ฟังที่ `127.0.0.1:8081` เท่านั้น
- ดู log: `./mac-native.sh logs` · อัปเดตโค้ด: คัดลอก `app.py` ใหม่ทับ แล้ว `./mac-native.sh install` อีกครั้ง
- กลับไปใช้ Docker: `./mac-native.sh uninstall` แล้วตั้ง `COMPOSE_PROFILES=cpu` และ `NGROK_UPSTREAM=embed:8080` → `docker compose up -d`

## ดูแลรักษา

| งาน | คำสั่ง |
|---|---|
| ดูสถานะ | `docker compose ps` (`docask-embed` ต้องเป็น `healthy` และ `docask-ngrok` ต้องเป็น `running`) · โหมดเร็ว: `./mac-native.sh status` |
| ดู log | `docker compose logs --tail 50` |
| อัปเดตโค้ด | คัดลอกไฟล์ใหม่ทับ แล้ว `docker compose up -d --build` |
| เปลี่ยน token | แก้ `.env` → `docker compose up -d` → ใส่ token ใหม่ใน DocAsk |
| IP ที่ทำงานเปลี่ยน | แก้ `NGROK_ALLOW_IPS` ใน `.env` → `docker compose up -d` |
| หยุด | `docker compose down` (โมเดลยังอยู่ใน volume) |

container มี `restart: unless-stopped` จึงกลับมาทำงานเองหลังรีสตาร์ทเครื่อง ตราบใดที่ Docker Desktop ตั้งให้เปิดตอน login

## ถ้าใช้ไม่ได้

| อาการใน DocAsk | ตรวจ |
|---|---|
| ติดต่อเซิร์ฟเวอร์ embedding ไม่ได้ | URL ของ ngrok เปลี่ยนหรือเปล่า (ดูข้อ 3) · Mac หลับหรือปิดอยู่ไหม · `docker compose ps` |
| ได้ 403 / ERR_NGROK_3208 | IP ของที่ทำงานไม่อยู่ใน `NGROK_ALLOW_IPS` (IP อาจเปลี่ยน) |
| ngrok restart วน | `NGROK_AUTHTOKEN` ผิด หรือ ngrok ตัวอื่นในบัญชีเดียวกันรันอยู่แล้ว ดู `docker compose logs ngrok` |
| token ไม่ถูกต้อง | token ใน DocAsk ตรงกับ `EMBED_TOKEN` ใน `.env` ไหม (ถ้าแก้ `.env` ต้อง `docker compose up -d` ใหม่) |
| เซิร์ฟเวอร์ไม่มีโมเดล … | เปลี่ยน `EMBED_MODEL` แล้วหรือเปล่า ให้กดทดสอบและ "ใช้เซิร์ฟเวอร์นี้" ใหม่ |
| container restart วน | RAM ของ Docker ไม่พอ ให้เพิ่มเป็น 6 GB ขึ้นไป |

## ความปลอดภัย

- ห้าม commit ไฟล์ `.env` (อยู่ใน `.gitignore` แล้ว)
- ห้ามเปลี่ยน port เป็น `0.0.0.0:8080` หรือเปิด port ที่ router เพราะข้อความในเอกสารของบริษัทจะวิ่งผ่านเส้นนี้
- URL ของ ngrok เปิดสู่อินเทอร์เน็ต ควรตั้ง `NGROK_ALLOW_IPS` เสมอ และอย่าเผยแพร่ URL หรือ token
- ngrok ถอด HTTPS ที่เซิร์ฟเวอร์ของ ngrok ข้อความจากเอกสารจึงผ่านบริษัท ngrok ในรูปที่อ่านได้ (Tailscale ไม่เป็นแบบนี้) ให้ตรวจว่านโยบายบริษัทยอมรับได้
- แผนฟรีของ ngrok มีโควตา request และปริมาณข้อมูลต่อเดือน ถ้า index เอกสารเยอะอาจชนเพดาน ดูการใช้งานได้ที่ dashboard
- DocAsk ยอมใช้ `http://` เฉพาะกับ localhost และที่อยู่ Tailscale (`100.64.0.0/10`, `*.ts.net`) ที่อื่นต้องเป็น `https://`

## ตั้งค่าเพิ่มเติม (`.env`)

| ตัวแปร | ค่าเริ่มต้น | ความหมาย |
|---|---|---|
| `EMBED_TOKEN` | (ต้องใส่) | ยาวอย่างน้อย 16 ตัว |
| `NGROK_AUTHTOKEN` | (ต้องใส่) | authtoken จาก ngrok dashboard |
| `NGROK_DOMAIN` | ว่าง | static domain จาก dashboard ว่าง = ngrok เลือก URL ให้ (อาจเปลี่ยนเมื่อรีสตาร์ท) |
| `NGROK_ALLOW_IPS` | ว่าง | CIDR ที่ยอมให้เข้า คั่นด้วย `,` ว่าง = ไม่กรอง IP |
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
