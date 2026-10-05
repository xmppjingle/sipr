FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --no-default-features -p siphone

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        tcpdump \
        iproute2 \
        iputils-ping \
        python3 \
        procps \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/siphone /usr/local/bin/siphone
WORKDIR /tmp
ENTRYPOINT ["sleep", "infinity"]
