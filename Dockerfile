FROM rust:1.51.0-buster

WORKDIR /linkerd-failover
COPY ./src ./src
COPY ./Cargo.* .
RUN cargo build

ENTRYPOINT ["/linkerd-failover/target/debug/linkerd-failover"]
