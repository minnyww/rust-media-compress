# media-compressor — Cloud Run service บีบอัดรูป/วิดีโอ ด้วย Rust

Service ภาษา **Rust (axum)** deploy บน **Cloud Run** ใน project `chayen-2` ชื่อ service `media-compressor` (region `asia-southeast1`)

- **รูป** — decode ด้วย crate `image` แล้วเข้ารหัสใหม่เป็น JPEG (ปรับ quality ได้) / WebP lossy (ผ่าน libwebp) / PNG
- **วิดีโอ** — สั่ง `ffmpeg` (ติดตั้งใน container) แปลงเป็น H.264 + AAC ปรับ CRF / ย่อขนาดได้

## Endpoints

| Endpoint | วิธีใช้ |
|---|---|
| `GET /` | คู่มือการใช้งาน (JSON) |
| `GET /health` | สถานะ + มี ffmpeg ไหม |
| `POST /compress/image` | multipart: `file` + `quality`(1-100, default 82) + `max_width`(px) + `format`(auto\|jpeg\|png\|webp) |
| `POST /compress/video` | multipart: `file` + `crf`(18-40, default 28) + `preset`(default veryfast) + `max_width`(px, 0=ไม่ย่อ) |

โหมด `auto` ของรูป: มี alpha → PNG, ไม่มี → JPEG และถ้าบีบแล้วได้ไฟล์ใหญ่กว่าต้นฉบับ จะคืนไฟล์เดิมให้เลย

**Response headers:** `x-original-bytes`, `x-compressed-bytes`, `x-saved-percent`, `x-elapsed-ms` (ไฟล์ผลลัพธ์อยู่ใน body, ชื่อไฟล์ `ชื่อเดิม.min.jpg` ฯลฯ)

## ตัวอย่างการใช้งาน

```bash
URL="https://media-compressor-<รหัส>-a.a.run.app"   # ดู URL จริงด้านล่าง

# บีบรูปเป็น WebP quality 75
curl -o out.webp -F "file=@photo.jpg" -F "format=webp" -F "quality=75" "$URL/compress/image"

# ย่อ + บีบรูปให้กว้างไม่เกิน 1200px
curl -o out.jpg -F "file=@photo.jpg" -F "max_width=1200" "$URL/compress/image"

# บีบวิดีโอ CRF 28 (ยิ่งสูงยิ่งเล็ก) + ย่อไม่เกิน 1280px
curl -o out.mp4 -F "file=@clip.mp4" -F "crf=28" -F "max_width=1280" "$URL/compress/video"
```

## URL ที่ deploy แล้ว

ด้วยคำสั่ง:

```bash
gcloud run deploy media-compressor \
  --source . \
  --project chayen-2 \
  --region asia-southeast1 \
  --allow-unauthenticated \
  --timeout=900 --cpu=2 --memory=1Gi --concurrency=4 --max-instances=2
```

> **URL จริง:** https://media-compressor-762018970351.asia-southeast1.run.app
> (deploy เมื่อ 2026-10-01, revision `media-compressor-00001-ntb`; ดู URL ล่าสุดด้วย `gcloud run services describe media-compressor --region asia-southeast1 --format='value(status.url)'`)

## รันบนเครื่อง

```bash
# แบบมี ffmpeg ในระบบ
cargo run --release

# แบบใช้ ffmpeg static ที่เก็บใน bin/ (โหลดจาก github eugeneware/ffmpeg-static)
FFMPEG_PATH=$PWD/bin/ffmpeg PORT=8090 ./target/release/media-compressor
```

ไฟล์ทดสอบใน `testfiles/` สร้างจาก ffmpeg lavfi (`photo.jpg` 4032×3024, `test.mp4` 1080p 8 วิ)

## ตัวเลขที่วัดได้จริง (Mac Mini M6)

| งาน | ผลลัพธ์ |
|---|---|
| รูป 23.3MB → WebP q70 | เหลือ 8.0MB (ประหยัด 66%) ~1,050ms |
| รูป 23.3MB → ย่อ 800px | เหลือ 259KB (99%) |
| วิดีโอ 10.1MB 1080p → CRF 28 | เหลือ 2.8MB (73%) ~410ms |
| วิดีโอ 10.1MB → 720p | เหลือ 318KB (97%) |

เทียบ Node.js (spawn ffmpeg / sharp): ความเร็วบีบไฟล์เท่ากัน แต่ memory idle 3MB vs 48MB และ cold start ~51ms vs 67ms+

## ข้อจำกัด / หมายเหตุ

- **Cloud Run จำกัด request 32MB** — ไฟล์ใหญ่กว่านั้นต้องเปลี่ยนเป็น pattern อัปโหลด GCS ก่อนแล้วให้ service ดึงจาก bucket
- ffmpeg ใน service มี timeout 600 วินาที (ตอน deploy ตั้ง request timeout ไว้ 900s)
- service เปิด `--allow-unauthenticated` = ใครมี URL ก็เรียกได้ ถ้าจะใช้จริงเชิงพาณิชย์ควรใส่ auth (IAM / ตรวจ token เอง)
- ค่า `bin/ffmpeg` (static ~80MB, สำหรับทดสอบ local เท่านั้น) — ใน container ของ Cloud Run ใช้ ffmpeg จาก apt ของ Debian ไม่ได้ใช้ไฟล์นี้ ลบทิ้งได้ถ้าไม่ต้องการทดสอบ local
# rust-media-compress
