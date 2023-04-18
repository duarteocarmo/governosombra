FROM rust:latest as builder

RUN apt-get update && apt-get install -y clang libssl-dev ffmpeg

WORKDIR /usr/src/governosombra
COPY Cargo.toml Cargo.lock ./
COPY transcripts/ ./transcripts/
COPY templates/ ./templates/
COPY episodes/ ./episodes/
COPY src/ ./src/
COPY download-ggml-model.sh ./download-ggml-model.sh

RUN cargo build --release
RUN ./download-ggml-model.sh base

# # Use a minimal image as the base
# FROM debian:buster-slim

# # Copy the app from the previous image
# COPY --from=builder /usr/src/governosombra/target/release/app /usr/local/bin/app
# COPY --from=builder /usr/src/governosombra/ggml-base.bin /usr/local/bin/ggml-base.bin

# Expose the port that the app listens on
EXPOSE 8080

# CMD ["app"]

CMD ["./target/release/app"]
