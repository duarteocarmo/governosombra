FROM rust:bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends clang cmake curl libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/governosombra

COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
COPY download-ggml-model.sh ./download-ggml-model.sh

RUN cargo build --release
RUN ./download-ggml-model.sh base

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates ffmpeg libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/governosombra

COPY --from=builder /usr/src/governosombra/target/release/app ./app
COPY --from=builder /usr/src/governosombra/ggml-base.bin ./ggml-base.bin
COPY templates/ ./templates/
COPY static/ ./static/

RUN mkdir -p episodes transcripts

EXPOSE 8080
CMD ["./app"]
