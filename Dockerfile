FROM rust:1.86-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p cogwheel-server

FROM debian:bookworm-slim
RUN useradd --system --create-home --uid 10001 cogwheel
WORKDIR /app
COPY --from=builder /app/target/release/cogwheel-server /usr/local/bin/cogwheel-server
USER cogwheel
EXPOSE 8080 53/udp 53/tcp
CMD ["cogwheel-server"]
