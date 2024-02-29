FROM rust:1.73 as builder
RUN apt update && apt install -y cmake libsasl2-dev

WORKDIR /usr/src/app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt update && apt install -y libsasl2-dev ca-certificates

COPY --from=builder /usr/src/app/target/release/product-import-kafka /usr/bin/product-import-kafka

CMD [ "product-import-kafka" ]
