# ---- build stage ----
FROM rust:1-slim-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# ---- runtime stage ----
# ต้องเป็น debian เวอร์ชันเดียวกับ build stage (bookworm) เพื่อให้ glibc ตรงกัน
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/media-compressor /usr/local/bin/media-compressor
ENV PORT=8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/media-compressor"]
