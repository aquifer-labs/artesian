# SPDX-License-Identifier: Apache-2.0

FROM rust:trixie AS builder

WORKDIR /src
COPY . .
RUN cargo build --release -p artesian-cli --features qdrant --bins

FROM debian:trixie-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --system --create-home --home-dir /var/lib/artesian --shell /usr/sbin/nologin artesian \
    && mkdir -p /data \
    && chown artesian:artesian /data
WORKDIR /data

COPY --from=builder /src/target/release/artesian /usr/local/bin/artesian
COPY --from=builder /src/target/release/artesiand /usr/local/bin/artesiand

USER artesian
VOLUME ["/data"]
ENTRYPOINT ["artesiand"]
CMD ["--config", "/data/artesian.toml", "--root", "/data"]
