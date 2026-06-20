FROM rust:latest as builder
RUN apt-get update && apt-get install -y clang libssl-dev ffmpeg cmake
WORKDIR /usr/src/governosombra
COPY Cargo.toml Cargo.lock ./
COPY transcripts/ ./transcripts/
COPY templates/ ./templates/
COPY episodes/ ./episodes/
COPY src/ ./src/
COPY download-ggml-model.sh ./download-ggml-model.sh
RUN cargo build --release
RUN ./download-ggml-model.sh base
EXPOSE 8080
CMD ["./target/release/app"]
