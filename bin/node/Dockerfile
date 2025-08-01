FROM rust:1.87-slim-bullseye AS builder

RUN apt-get update && \
    apt-get -y upgrade && \
    apt-get install -y llvm clang bindgen pkg-config libssl-dev libsqlite3-dev ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY ./Cargo.toml .
COPY ./Cargo.lock .
COPY ./bin ./bin
COPY ./crates ./crates
COPY ./proto ./proto

RUN cargo install --path bin/node --locked
RUN rm -rf data accounts && mkdir data accounts
RUN miden-node bundled bootstrap --data-directory ./data --accounts-directory ./accounts

FROM debian:bullseye-slim

# Update machine & install required packages
# The installation of sqlite3 is needed for correct function of the SQLite database
RUN apt-get update && \
    apt-get -y upgrade && \
    apt-get install -y --no-install-recommends \
    sqlite3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/accounts accounts
COPY --from=builder /app/data data
COPY --from=builder /usr/local/cargo/bin/miden-node /usr/local/bin/miden-node

LABEL org.opencontainers.image.authors=devops@miden.team \
    org.opencontainers.image.url=https://0xMiden.github.io/ \
    org.opencontainers.image.documentation=https://github.com/0xMiden/miden-node \
    org.opencontainers.image.source=https://github.com/0xMiden/miden-node \
    org.opencontainers.image.vendor=Miden \
    org.opencontainers.image.licenses=MIT

ARG CREATED
ARG VERSION
ARG COMMIT
LABEL org.opencontainers.image.created=$CREATED \
    org.opencontainers.image.version=$VERSION \
    org.opencontainers.image.revision=$COMMIT

# Expose RPC port
EXPOSE 57291

# Start the Miden node
# Miden node does not spawn sub-processes, so it can be used as the PID1
CMD miden-node bundled start --rpc.url http://0.0.0.0:57291 --data-directory ./data
