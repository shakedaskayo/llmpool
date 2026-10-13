# The llmpool team server: the command line tool, serving a pool.
FROM rust:1-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends libdbus-1-dev pkg-config && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p llmpool-cli

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libdbus-1-3 && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --home-dir /data --no-create-home llmpool \
    && mkdir /data && chown 10001:10001 /data
COPY --from=build /src/target/release/llmpool /usr/local/bin/llmpool
USER 10001
# A container has no keychain: logins live in files under /data, which must be
# a volume that is encrypted at rest and readable by nobody else.
ENV HOME=/data \
    LLMPOOL_HOME=/data \
    LLMPOOL_FILE_VAULT=1 \
    LLMPOOL_NO_NOTIFY=1
VOLUME /data
EXPOSE 8787
ENTRYPOINT ["llmpool"]
CMD ["serve", "--listen", "0.0.0.0:8787"]
