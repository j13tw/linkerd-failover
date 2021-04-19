FROM rust:1.51.0-buster as builder

WORKDIR /usr/src/linkerd-failover
COPY ./src ./src
COPY ./Cargo.* .
RUN cargo install --path .

FROM debian:buster-slim
RUN apt-get update && apt-get install -y libssl1.1 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/cargo/bin/linkerd-failover /usr/local/bin/linkerd-failover
ENTRYPOINT ["linkerd-failover"]
