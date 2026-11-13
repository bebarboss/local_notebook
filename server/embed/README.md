# DocAsk embedding server

เซิร์ฟเวอร์ทำ embedding ให้ DocAsk รันใน Docker บนเครื่องที่บ้าน (เขียนไว้สำหรับ Mac mini Apple Silicon และใช้กับเครื่อง Linux หรือ Windows ได้ด้วย) แล้วต่อจากที่ทำงานผ่าน Cloudflare Quick Tunnel (HTTPS, ฟรี ไม่ต้องมีบัญชีหรือโดเมน)

- ใช้โมเดล `BAAI/bge-m3` (1024 มิติ, รองรับภาษาไทย) แบบ fp32 เต็มความแม่น ไม่ใช่ int8 แบบที่แอปใช้ในเครื่อง
- API เป็นแบบเดียวกับ OpenAI `POST /v1/embeddings` และต้องส่ง `Authorization: Bearer <token>`
- ไม่เก็บและไม่ log ข้อความที่ส่งมา log แค่จำนวนข้อความกับเวลาที่ใช้

## สิ่งที่ต้องมีบน Mac mini

- **Docker Desktop:** ตั้งค่า › General › เปิด *Start Docker Desktop when you sign in* และตั้งค่า › Resources › Memory **อย่างน้อย 6 GB**
  - ใช้ OrbStack แทนได้ กิน RAM น้อยกว่า
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

**3. ดู URL ของ tunnel**

container `docask-tunnel` (cloudflared) เปิดพร้อมกันจากข้อ 2 แล้ว

```bash
docker compose logs tunnel | grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' | tail -1
curl https://<URL ที่ได้>/health
```

**URL เปลี่ยนทุกครั้งที่ container `docask-tunnel` รีสตาร์ท** (รีบูตเครื่อง, Docker รีสตาร์ท, `docker compose up -d` ที่สร้าง container ใหม่) ถ้า DocAsk ขึ้นว่าติดต่อเซิร์ฟเวอร์ไม่ได้ ให้รันคำสั่งข้างบนแล้วใส่ URL ใหม่ใน DocAsk

port 8080/8081 ผูกไว้ที่ `127.0.0.1` เท่านั้น เครื่องอื่นใน LAN เข้าตรงไม่ได้ ช่องทางเดียวจากข้างนอกคือ tunnel ซึ่งส่งต่อให้เซิร์ฟเวอร์ตรวจ token

**4. ตั้งค่าใน DocAsk (เครื่อง Windows)**

1. เปิด ตั้งค่า › Embedding และ Index › **หรือใช้เซิร์ฟเวอร์ embedding ของตัวเอง**
2. ใส่ URL จากข้อ 3 และ token จากข้อ 1 แล้วกด **บันทึกและทดสอบ**
3. ถ้าขึ้นว่า "ใช้ได้" ให้กด **ใช้เซิร์ฟเวอร์นี้** แอปจะทำ embedding ใหม่ทุก service

token ถูกเก็บใน Windows Credential Manager ไม่ได้เก็บในไฟล์

**ฝัง URL + token มากับโปรแกรม (v0.12.1 ขึ้นไป)** — GitHub repo › Settings › Secrets and variables › Actions ›
เพิ่ม secret `DOCASK_EMBED_URL` (**ต้องเป็น URL ที่ไม่เปลี่ยน** — Quick Tunnel ใช้ไม่ได้ ต้องเป็น Cloudflare Named Tunnel บนโดเมนของตัวเอง) และ
`DOCASK_EMBED_TOKEN` (= `EMBED_TOKEN`) แล้วออกเวอร์ชันใหม่ · เครื่องที่ยังไม่ได้ดาวน์โหลด bge-m3 จะใช้เซิร์ฟเวอร์นี้เอง ·
token ที่ฝังมาเปลี่ยนในแอปไม่ได้ เปลี่ยนได้แค่ URL · ไม่อยู่ในโค้ดของ repo แต่**ไฟล์ติดตั้งเป็น public ใครก็แกะ token ออกได้**
→ ควรจำกัด IP ด้วย (Named Tunnel + Cloudflare WAF rule) · เปลี่ยน token: แก้ `.env` +
`docker compose up -d` + แก้ secret แล้วออกเวอร์ชันใหม่

**5. (แนะนำ) โมเดลสำรองในเครื่อง** — ใต้ผลทดสอบกด **ดาวน์โหลด bge-m3 ไว้สำรอง** (~570 MB) ถ้า Mac ปิดหรือเน็ตหลุด
DocAsk จะคำนวณคำถามในเครื่องแทนและยังค้นเอกสารได้ (index อยู่บน Windows อยู่แล้ว) · ไฟล์ใหม่จะรอ index จนเซิร์ฟเวอร์กลับมา

## โหมดเร็ว (Mac Apple Silicon, GPU)

รันเซิร์ฟเวอร์บน macOS ตรงๆ ให้ใช้ GPU (Metal) ได้ เร็วกว่า Docker ราว 6–10 เท่า (M2: ราว 3 วินาทีต่อ 32 ท่อน) และได้ vector เดิมทุกตัว ไม่ต้อง index ใหม่ tunnel ยังรันใน Docker เหมือนเดิม

```bash
./mac-native.sh install      # ครั้งแรกติดตั้ง Python 3.12 + PyTorch ใน ~/.docask-embed และดาวน์โหลดโมเดล ~2.3 GB
./mac-native.sh status       # ต้องเห็น "device":"mps:0"
```

แล้วแก้ `.env` ให้ tunnel ส่งต่อไปที่เซิร์ฟเวอร์ตัวนี้ และปิด container CPU

```ini
COMPOSE_PROFILES=
TUNNEL_UPSTREAM=host.docker.internal:8081
```

```bash
docker compose up -d tunnel
docker compose --profile cpu rm -sf embed
```

URL ของ tunnel จะเปลี่ยน ให้ใส่ URL ใหม่ใน DocAsk ตามข้อ 3–4

- เซิร์ฟเวอร์เริ่มเองตอน login ผ่าน launchd (`~/Library/LaunchAgents/com.docask.embed.plist`) และเปิดใหม่เองถ้าล่ม ฟังที่ `127.0.0.1:8081` เท่านั้น
- ดู log: `./mac-native.sh logs` · อัปเดตโค้ด: คัดลอก `app.py` ใหม่ทับ แล้ว `./mac-native.sh install` อีกครั้ง
- กลับไปใช้ Docker: `./mac-native.sh uninstall` แล้วตั้ง `COMPOSE_PROFILES=cpu` และ `TUNNEL_UPSTREAM=embed:8080` → `docker compose up -d`

## ดูแลรักษา

| งาน | คำสั่ง |
|---|---|
| ดูสถานะ | `docker compose ps` (`docask-embed` ต้องเป็น `healthy` และ `docask-tunnel` ต้องเป็น `running`) · โหมดเร็ว: `./mac-native.sh status` |
| ดู log | `docker compose logs --tail 50` |
| อัปเดตโค้ด | คัดลอกไฟล์ใหม่ทับ แล้ว `docker compose up -d --build` |
| เปลี่ยน token | แก้ `.env` → `docker compose up -d` → ใส่ token ใหม่ใน DocAsk |
| หยุด | `docker compose down` (โมเดลยังอยู่ใน volume) |

container มี `restart: unless-stopped` จึงกลับมาทำงานเองหลังรีสตาร์ทเครื่อง ตราบใดที่ Docker Desktop ตั้งให้เปิดตอน login

## ถ้าใช้ไม่ได้

| อาการใน DocAsk | ตรวจ |
|---|---|
| ติดต่อเซิร์ฟเวอร์ embedding ไม่ได้ | URL ของ tunnel เปลี่ยนหรือเปล่า (ดูข้อ 3) · Mac หลับหรือปิดอยู่ไหม · `docker compose ps` |
| ได้ 502 / 530 จาก Cloudflare | tunnel ต่อเซิร์ฟเวอร์ไม่ได้ ดู `docker compose logs tunnel` และเซิร์ฟเวอร์ยังรันอยู่ไหม (`./mac-native.sh status`) |
| token ไม่ถูกต้อง | token ใน DocAsk ตรงกับ `EMBED_TOKEN` ใน `.env` ไหม (ถ้าแก้ `.env` ต้อง `docker compose up -d` ใหม่) |
| เซิร์ฟเวอร์ไม่มีโมเดล … | เปลี่ยน `EMBED_MODEL` แล้วหรือเปล่า ให้กดทดสอบและ "ใช้เซิร์ฟเวอร์นี้" ใหม่ |
| container restart วน | RAM ของ Docker ไม่พอ ให้เพิ่มเป็น 6 GB ขึ้นไป |

## ความปลอดภัย

- ห้าม commit ไฟล์ `.env` (อยู่ใน `.gitignore` แล้ว)
- ห้ามเปลี่ยน port เป็น `0.0.0.0:8080` หรือเปิด port ที่ router เพราะข้อความในเอกสารของบริษัทจะวิ่งผ่านเส้นนี้
- URL ของ Quick Tunnel เปิดสู่อินเทอร์เน็ตและจำกัด IP ไม่ได้ ด่านเดียวคือ token อย่าเผยแพร่ URL หรือ token
- Cloudflare ถอด HTTPS ที่เซิร์ฟเวอร์ของ Cloudflare ข้อความจากเอกสารจึงผ่าน Cloudflare ในรูปที่อ่านได้ (Tailscale ไม่เป็นแบบนี้) ให้ตรวจว่านโยบายบริษัทยอมรับได้
- Quick Tunnel ไม่มีโควตารายเดือน แต่ Cloudflare ระบุว่าไว้ทดสอบ ไม่รับประกัน uptime ถ้าต้องการ URL คงที่และจำกัด IP ให้ใช้ Named Tunnel บนโดเมนของตัวเอง
- DocAsk ยอมใช้ `http://` เฉพาะกับ localhost และที่อยู่ Tailscale (`100.64.0.0/10`, `*.ts.net`) ที่อื่นต้องเป็น `https://`

## ตั้งค่าเพิ่มเติม (`.env`)

| ตัวแปร | ค่าเริ่มต้น | ความหมาย |
|---|---|---|
| `EMBED_TOKEN` | (ต้องใส่) | ยาวอย่างน้อย 16 ตัว |
| `COMPOSE_PROFILES` | `cpu` | `cpu` = รันเซิร์ฟเวอร์ใน Docker · ว่าง = โหมดเร็ว (`./mac-native.sh`) |
| `TUNNEL_UPSTREAM` | `embed:8080` | ที่ที่ tunnel ส่งต่อไป · โหมดเร็ว = `host.docker.internal:8081` |
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
