use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Multipart},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde_json::json;
use tokio::io::AsyncWriteExt;

const MAX_BODY_BYTES: usize = 200 * 1024 * 1024;
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(600);

static FFMPEG_AVAILABLE: OnceLock<bool> = OnceLock::new();

fn ffmpeg_bin() -> String {
    std::env::var("FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_string())
}

struct AppError(StatusCode, String);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}

fn bad(msg: impl Into<String>) -> AppError {
    AppError(StatusCode::BAD_REQUEST, msg.into())
}

fn unsupported(msg: impl Into<String>) -> AppError {
    AppError(StatusCode::UNSUPPORTED_MEDIA_TYPE, msg.into())
}

fn internal(msg: impl Into<String>) -> AppError {
    AppError(StatusCode::INTERNAL_SERVER_ERROR, msg.into())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let ffmpeg_ok = tokio::process::Command::new(ffmpeg_bin())
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    FFMPEG_AVAILABLE.set(ffmpeg_ok).ok();
    if ffmpeg_ok {
        tracing::info!("ffmpeg found at `{}`", ffmpeg_bin());
    } else {
        tracing::warn!(
            "ffmpeg NOT found at `{}` — /compress/video will return 500",
            ffmpeg_bin()
        );
    }

    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/compress/image", post(compress_image))
        .route("/compress/video", post(compress_video))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES));

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("failed to bind");
    tracing::info!("listening on 0.0.0.0:{port}");
    axum::serve(listener, app).await.unwrap();
}

async fn index() -> Response {
    axum::Json(json!({
        "service": "media-compressor",
        "endpoints": {
            "POST /compress/image": {
                "multipart/form-data fields": {
                    "file": "ไฟล์รูป (jpeg/png/webp/gif/bmp/tiff)",
                    "quality": "1-100 (default 82) — ใช้กับ jpeg/webp",
                    "max_width": "px — ย่อภาพถ้ากว้างหรือสูงเกิน (optional)",
                    "format": "auto | jpeg | png | webp (default auto: มี alpha → png, ไม่มี → jpeg)"
                }
            },
            "POST /compress/video": {
                "multipart/form-data fields": {
                    "file": "ไฟล์วิดีโอ (mp4/mov/webm/mkv/...)",
                    "crf": "18-40 — ยิ่งสูงยิ่งไฟล์เล็ก (default 28)",
                    "preset": "ultrafast|superfast|veryfast|faster|fast|medium|slow (default veryfast)",
                    "max_width": "px — 0 = ไม่ย่อ (default 0)"
                }
            },
            "GET /health": "status + มี ffmpeg ไหม"
        },
        "response_headers": {
            "x-original-bytes": "ขนาดไฟล์ต้นฉบับ",
            "x-compressed-bytes": "ขนาดไฟล์ผลลัพธ์",
            "x-saved-percent": "% ที่เล็กลง"
        }
    }))
    .into_response()
}

async fn health() -> Response {
    axum::Json(json!({
        "ok": true,
        "ffmpeg": FFMPEG_AVAILABLE.get().copied().unwrap_or(false),
    }))
    .into_response()
}

// ---------- image ----------

enum OutFormat {
    Jpeg,
    Png,
    Webp,
}

impl OutFormat {
    fn mime(&self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
        }
    }
    fn ext(&self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }
}

fn mime_for(f: image::ImageFormat) -> &'static str {
    use image::ImageFormat::*;
    match f {
        Jpeg => "image/jpeg",
        Png => "image/png",
        WebP => "image/webp",
        Gif => "image/gif",
        Bmp => "image/bmp",
        Tiff => "image/tiff",
        _ => "application/octet-stream",
    }
}

fn ext_for(f: image::ImageFormat) -> &'static str {
    use image::ImageFormat::*;
    match f {
        Jpeg => "jpg",
        Png => "png",
        WebP => "webp",
        Gif => "gif",
        Bmp => "bmp",
        Tiff => "tiff",
        _ => "bin",
    }
}

fn encode_image(img: image::DynamicImage, want: OutFormat, quality: u8) -> Result<Vec<u8>, AppError> {
    let mut out = Vec::new();
    match want {
        OutFormat::Jpeg => {
            let rgb = image::DynamicImage::ImageRgb8(img.to_rgb8());
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
            rgb.write_with_encoder(enc)
                .map_err(|e| internal(format!("jpeg encode: {e}")))?;
        }
        OutFormat::Png => {
            let enc = image::codecs::png::PngEncoder::new_with_quality(
                &mut out,
                image::codecs::png::CompressionType::Best,
                image::codecs::png::FilterType::Adaptive,
            );
            img.write_with_encoder(enc)
                .map_err(|e| internal(format!("png encode: {e}")))?;
        }
        OutFormat::Webp => {
            // libwebp lossy ผ่าน crate `webp` (image crate เข้ารหัส webp ได้แบบ lossless เท่านั้น)
            let flat = if img.color().has_alpha() {
                image::DynamicImage::ImageRgba8(img.to_rgba8())
            } else {
                image::DynamicImage::ImageRgb8(img.to_rgb8())
            };
            let mem = webp::Encoder::from_image(&flat)
                .map_err(|e| internal(format!("webp prepare: {e}")))?
                .encode(f32::from(quality));
            out.extend_from_slice(&mem);
        }
    }
    Ok(out)
}

async fn read_text_field(
    field: axum::extract::multipart::Field<'_>,
    what: &str,
) -> Result<String, AppError> {
    field
        .text()
        .await
        .map_err(|e| bad(format!("read {what}: {e}")))
}

async fn compress_image(mut mp: Multipart) -> Result<Response, AppError> {
    let mut file: Option<Vec<u8>> = None;
    let mut filename = String::from("image");
    let mut quality: u8 = 82;
    let mut max_width: Option<u32> = None;
    let mut format = String::from("auto");

    while let Some(mut field) = mp
        .next_field()
        .await
        .map_err(|e| bad(format!("multipart error: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                if let Some(f) = field.file_name().map(|s| s.to_string()) {
                    filename = f;
                }
                let mut buf = Vec::new();
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|e| bad(format!("read file chunk: {e}")))?
                {
                    buf.extend_from_slice(&chunk);
                }
                file = Some(buf);
            }
            "quality" => {
                let t = read_text_field(field, "quality").await?;
                quality = t
                    .trim()
                    .parse()
                    .map_err(|_| bad("quality must be an integer 1-100"))?;
                if quality == 0 || quality > 100 {
                    return Err(bad("quality must be 1-100"));
                }
            }
            "max_width" => {
                let t = read_text_field(field, "max_width").await?;
                let w: u32 = t
                    .trim()
                    .parse()
                    .map_err(|_| bad("max_width must be a number (px)"))?;
                if !(16..=16384).contains(&w) {
                    return Err(bad("max_width must be 16-16384"));
                }
                max_width = Some(w);
            }
            "format" => {
                let t = read_text_field(field, "format").await?;
                format = t.trim().to_ascii_lowercase();
                if !["auto", "jpeg", "jpg", "png", "webp"].contains(&format.as_str()) {
                    return Err(bad("format must be auto | jpeg | png | webp"));
                }
            }
            _ => {}
        }
    }

    let data = file.ok_or_else(|| bad("missing multipart field `file`"))?;
    if data.is_empty() {
        return Err(bad("empty file"));
    }

    let t0 = Instant::now();
    let src_format =
        image::guess_format(&data).map_err(|e| unsupported(format!("unrecognized image: {e}")))?;

    let mut img =
        image::load_from_memory(&data).map_err(|e| unsupported(format!("decode failed: {e}")))?;

    if let Some(mw) = max_width {
        if img.width() > mw || img.height() > mw {
            img = img.thumbnail(mw, mw);
        }
    }

    let has_alpha = img.color().has_alpha();
    let want = match format.as_str() {
        "jpeg" | "jpg" => OutFormat::Jpeg,
        "png" => OutFormat::Png,
        "webp" => OutFormat::Webp,
        _ => {
            if has_alpha {
                OutFormat::Png
            } else {
                OutFormat::Jpeg
            }
        }
    };

    let out_mime = want.mime();
    let out_ext = want.ext();
    let out = encode_image(img, want, quality)?;

    let orig_len = data.len();
    // โหมด auto: ถ้าเข้ารหัสใหม่แล้วไม่เล็กกว่าเดิม คืนไฟล์ต้นฉบับไปเลย
    let return_original = format == "auto" && out.len() >= orig_len;
    let (bytes, mime, ext) = if return_original {
        (data, mime_for(src_format), ext_for(src_format))
    } else {
        (out, out_mime, out_ext)
    };
    let note = if return_original { "original" } else { "compressed" };

    tracing::info!(
        "image {} -> {} ({}B -> {}B, {}ms)",
        filename,
        mime,
        orig_len,
        bytes.len(),
        t0.elapsed().as_millis()
    );

    Ok(file_response(
        bytes,
        mime,
        &out_filename(&filename, ext),
        orig_len,
        t0.elapsed().as_millis(),
        note,
    ))
}

// ---------- video ----------

async fn compress_video(mut mp: Multipart) -> Result<Response, AppError> {
    if !FFMPEG_AVAILABLE.get().copied().unwrap_or(false) {
        return Err(internal("ffmpeg is not available in this environment"));
    }

    let mut filename = String::from("video");
    let mut crf: u32 = 28;
    let mut preset = String::from("veryfast");
    let mut max_width: u32 = 0;

    let dir = tempfile::tempdir().map_err(|e| internal(format!("tempdir: {e}")))?;
    let in_path = dir.path().join("input.bin");
    let out_path = dir.path().join("output.mp4");
    let mut f = tokio::fs::File::create(&in_path)
        .await
        .map_err(|e| internal(format!("create temp file: {e}")))?;

    let mut file_written = false;
    while let Some(mut field) = mp
        .next_field()
        .await
        .map_err(|e| bad(format!("multipart error: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                if let Some(fn_) = field.file_name().map(|s| s.to_string()) {
                    filename = fn_;
                }
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|e| bad(format!("read file chunk: {e}")))?
                {
                    f.write_all(&chunk)
                        .await
                        .map_err(|e| internal(format!("write temp file: {e}")))?;
                }
                file_written = true;
            }
            "crf" => {
                let t = read_text_field(field, "crf").await?;
                let v: u32 = t
                    .trim()
                    .parse()
                    .map_err(|_| bad("crf must be a number"))?;
                if !(18..=40).contains(&v) {
                    return Err(bad("crf must be 18-40"));
                }
                crf = v;
            }
            "preset" => {
                let t = read_text_field(field, "preset").await?;
                preset = t.trim().to_ascii_lowercase();
                if ![
                    "ultrafast",
                    "superfast",
                    "veryfast",
                    "faster",
                    "fast",
                    "medium",
                    "slow",
                ]
                .contains(&preset.as_str())
                {
                    return Err(bad(
                        "preset must be ultrafast|superfast|veryfast|faster|fast|medium|slow",
                    ));
                }
            }
            "max_width" => {
                let t = read_text_field(field, "max_width").await?;
                let v: u32 = t
                    .trim()
                    .parse()
                    .map_err(|_| bad("max_width must be a number"))?;
                if v > 7680 {
                    return Err(bad("max_width must be 0-7680 (0 = no scaling)"));
                }
                max_width = v;
            }
            _ => {}
        }
    }

    if !file_written {
        return Err(bad("missing multipart field `file`"));
    }
    f.flush()
        .await
        .map_err(|e| internal(format!("flush temp file: {e}")))?;
    drop(f);

    let t0 = Instant::now();
    // -2 ใน scale = คูณรองให้เป็นเลขคู่ (h.264 ต้องการ yuv420p ขนาดเลขคู่)
    // เครื่องหมาย , ใน min() ต้อง escape เพราะใน filtergraph , คือตัวคั่นระหว่าง filter
    let scale = if max_width > 0 {
        format!("scale=min({max_width}\\,iw):-2")
    } else {
        "scale=trunc(iw/2)*2:trunc(ih/2)*2".to_string()
    };

    let run = tokio::process::Command::new(ffmpeg_bin())
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(&in_path)
        .args(["-c:v", "libx264"])
        .arg("-crf")
        .arg(crf.to_string())
        .args(["-preset", &preset, "-pix_fmt", "yuv420p", "-vf", &scale])
        .args([
            "-c:a",
            "aac",
            "-b:a",
            "96k",
            "-map_metadata",
            "-1",
            "-movflags",
            "+faststart",
        ])
        .arg(&out_path)
        .output();

    let output = tokio::time::timeout(FFMPEG_TIMEOUT, run)
        .await
        .map_err(|_| internal(format!("ffmpeg timed out after {}s", FFMPEG_TIMEOUT.as_secs())))?
        .map_err(|e| internal(format!("failed to run ffmpeg: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr
            .chars()
            .rev()
            .take(400)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Err(internal(format!("ffmpeg failed: {tail}")));
    }

    let orig_len = tokio::fs::metadata(&in_path)
        .await
        .map(|m| m.len() as usize)
        .unwrap_or(0);
    let bytes = tokio::fs::read(&out_path)
        .await
        .map_err(|e| internal(format!("read output: {e}")))?;
    if bytes.is_empty() {
        return Err(internal("ffmpeg produced an empty output"));
    }

    tracing::info!(
        "video {} crf={crf} preset={preset} ({}B -> {}B, {}ms)",
        filename,
        orig_len,
        bytes.len(),
        t0.elapsed().as_millis()
    );

    Ok(file_response(
        bytes,
        "video/mp4",
        &out_filename(&filename, "mp4"),
        orig_len,
        t0.elapsed().as_millis(),
        "compressed",
    ))
}

// ---------- shared ----------

fn out_filename(original: &str, ext: &str) -> String {
    let stem = original.rsplit_once('.').map(|(s, _)| s).unwrap_or(original);
    let stem: String = stem
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    format!("{stem}.min.{ext}")
}

fn file_response(
    bytes: Vec<u8>,
    mime: &str,
    out_filename: &str,
    orig_len: usize,
    elapsed_ms: u128,
    note: &str,
) -> Response {
    let compressed_len = bytes.len();
    let saved = if orig_len > 0 {
        (100.0 * (1.0 - compressed_len as f64 / orig_len as f64)).round() as i64
    } else {
        0
    };

    let mut resp = Response::new(Body::from(bytes));
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(mime) {
        h.insert(header::CONTENT_TYPE, v);
    }
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{out_filename}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    for (name, val) in [
        ("x-original-bytes", orig_len.to_string()),
        ("x-compressed-bytes", compressed_len.to_string()),
        ("x-saved-percent", saved.to_string()),
        ("x-elapsed-ms", elapsed_ms.to_string()),
        ("x-note", note.to_string()),
    ] {
        if let Ok(v) = HeaderValue::from_str(&val) {
            h.insert(name, v);
        }
    }
    resp
}
