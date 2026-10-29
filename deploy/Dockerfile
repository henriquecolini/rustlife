# syntax=docker/dockerfile:1
#
# The rustlife dedicated server, as a container, for x86-64 and ARM64 hosts.
#
# The image holds only rustlife's own code and the DepotDownloader Steam
# client: no game content. On start, the entrypoint downloads the Half-Life 2:
# Deathmatch Dedicated Server (Steam app 232370, anonymous login) into the
# `/data/hl2dm` volume and serves from it. See docs/deploy.md.
#
#     docker build -f deploy/Dockerfile -t rustlife-server .

# --- Build ------------------------------------------------------------------
# The workspace needs Rust 1.90+ (edition 2024); bookworm matches the runtime
# image's glibc.
FROM rust:1.99-bookworm AS build
WORKDIR /src
COPY . .
# The cache mounts keep the downloaded crates, the git dependency and the
# compiled dependencies between builds, so a code change rebuilds only
# rustlife's own crates. The target directory is a cache, so the binary is
# copied out of it in the same step.
RUN --mount=type=cache,id=rustlife-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=rustlife-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=rustlife-target,target=/src/target \
    cargo build --release --locked -p rustlife-server \
    && install -D target/release/rustlife-server /out/rustlife-server

# --- DepotDownloader --------------------------------------------------------
# SteamRE's DepotDownloader downloads Steam apps like SteamCMD does, but it
# is a self-contained .NET program built for ARM64 as well as x86-64, where
# SteamCMD is 32-bit x86 only. The release is pinned and checksummed.
FROM debian:bookworm-slim AS depotdownloader
ARG TARGETARCH
ARG DEPOTDOWNLOADER_VERSION=3.4.0
ARG DEPOTDOWNLOADER_SHA256_AMD64=a999dec66b4850fc961bd50366696d23c2d0fad7b18790e6a5647b2f19097a53
ARG DEPOTDOWNLOADER_SHA256_ARM64=d9fb612ccebc1db8eeea3b4045d2221ec70431381393ce908fb72f01d4f9c812
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl unzip \
    && rm -rf /var/lib/apt/lists/*
RUN case "$TARGETARCH" in \
        amd64) asset=linux-x64; sha256=$DEPOTDOWNLOADER_SHA256_AMD64 ;; \
        arm64) asset=linux-arm64; sha256=$DEPOTDOWNLOADER_SHA256_ARM64 ;; \
        *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && curl -fsSL -o /tmp/dd.zip \
        "https://github.com/SteamRE/DepotDownloader/releases/download/DepotDownloader_${DEPOTDOWNLOADER_VERSION}/DepotDownloader-${asset}.zip" \
    && echo "$sha256  /tmp/dd.zip" | sha256sum -c - \
    && unzip -q /tmp/dd.zip -d /opt/depotdownloader \
    && chmod 0755 /opt/depotdownloader/DepotDownloader \
    && rm /tmp/dd.zip

# --- Runtime ----------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# DepotDownloader needs OpenSSL for its TLS connections to Steam; the rest of
# the .NET runtime is in its binary.
# tini forwards `docker stop`'s SIGTERM to DepotDownloader or the server and
# reaps zombies, which a shell or a game server running as PID 1 would not do.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 tini \
    && rm -rf /var/lib/apt/lists/*

# An unprivileged user owns everything the server writes. The volume mount
# points are created here, owned by that user, so that fresh named volumes
# inherit the ownership.
ARG UID=1000
ARG GID=1000
RUN groupadd --gid "$GID" steam \
    && useradd --uid "$UID" --gid steam --create-home --home-dir /home/steam steam \
    && mkdir -p /data/hl2dm /config \
    && chown -R steam:steam /data /config /home/steam

COPY --from=depotdownloader /opt/depotdownloader /opt/depotdownloader
COPY --from=build /out/rustlife-server /usr/local/bin/rustlife-server
COPY --chmod=0755 deploy/entrypoint.sh /usr/local/bin/entrypoint.sh

USER steam
# NO_COLOR keeps the server's log free of terminal colour codes, which log
# viewers show as noise. Invariant globalization spares DepotDownloader the
# ICU library, which it would otherwise refuse to start without.
ENV HOME=/home/steam \
    RUST_LOG=info \
    NO_COLOR=1 \
    DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1
# The game content, the server config, and the steam user's home, where
# DepotDownloader keeps a remembered Steam login (for the fallback).
VOLUME ["/data/hl2dm", "/config", "/home/steam"]
EXPOSE 27015/udp
ENTRYPOINT ["/usr/bin/tini", "-g", "--", "/usr/local/bin/entrypoint.sh"]
