FROM rust:bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends clang cmake curl libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/governosombra

COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
COPY download-ggml-model.sh ./download-ggml-model.sh

# # ggml's native ARM CPU detection emits dotprod (sdot) instructions that the
# # assembler in this image rejects. Setting SOURCE_DATE_EPOCH makes ggml fall
# # back to a portable armv8-a build (see ggml/CMakeLists.txt).
# ENV SOURCE_DATE_EPOCH=0

RUN cargo build --release
RUN ./download-ggml-model.sh base

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl ffmpeg libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/governosombra

COPY --from=builder /usr/src/governosombra/target/release/app ./app
COPY --from=builder /usr/src/governosombra/ggml-base.bin ./ggml-base.bin
COPY templates/ ./templates/
COPY static/ ./static/

RUN mkdir -p episodes transcripts

EXPOSE 8080
CMD ["./app"]
