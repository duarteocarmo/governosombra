# Base stage with dependencies and cargo-chef
FROM rust:latest AS base
RUN apt-get update && apt-get install -y clang libssl-dev ffmpeg cmake
RUN cargo install cargo-chef --locked

# Planner stage - generates recipe.json
FROM base AS planner
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Builder stage - builds dependencies then source
FROM base AS builder
WORKDIR /app

# Build dependencies first (cached unless Cargo.toml/Cargo.lock change)
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Copy source and build
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
COPY templates/ ./templates/
COPY transcripts/ ./transcripts/
COPY episodes/ ./episodes/
RUN cargo build --release

# Download model
COPY download-ggml-model.sh ./download-ggml-model.sh
RUN ./download-ggml-model.sh base

# Runtime stage - same debian as rust:latest uses
FROM debian:trixie-slim AS runtime
RUN apt-get update && apt-get install -y libssl3 ffmpeg ca-certificates && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/app ./app
COPY --from=builder /app/ggml-base.bin ./ggml-base.bin
COPY --from=builder /app/templates ./templates
COPY --from=builder /app/transcripts ./transcripts
COPY --from=builder /app/episodes ./episodes

EXPOSE 8080
CMD ["./app"]
