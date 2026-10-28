# syntax=docker/dockerfile:1
#
# The rustlife dedicated server, as a container.
#
# The image holds only rustlife's own code and Valve's SteamCMD installer: no
# game content. On start, the entrypoint downloads the Half-Life 2: Deathmatch
# Dedicated Server (Steam app 232370, anonymous login) into the `/data/hl2dm`
# volume and serves from it. See docs/deploy.md.
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

# --- Runtime ----------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# SteamCMD is a 32-bit program: it needs the 32-bit C and C++ runtimes.
# tini forwards `docker stop`'s SIGTERM to SteamCMD or the server and reaps
# zombies, which a shell or a game server running as PID 1 would not do.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl lib32gcc-s1 lib32stdc++6 tini \
    && rm -rf /var/lib/apt/lists/*

# SteamCMD's bootstrapper. On the first start the entrypoint copies it into
# SteamCMD's volume, where it updates itself (about 200 MB) once rather than
# on every new container.
RUN mkdir -p /opt/steamcmd \
    && curl -fsSL https://steamcdn-a.akamaihd.net/client/installer/steamcmd_linux.tar.gz \
        | tar -xz -C /opt/steamcmd

# An unprivileged user owns everything the server writes. The volume mount
# points are created here, owned by that user, so that fresh named volumes
# inherit the ownership.
ARG UID=1000
ARG GID=1000
RUN groupadd --gid "$GID" steam \
    && useradd --uid "$UID" --gid steam --create-home --home-dir /home/steam steam \
    && mkdir -p /data/hl2dm /config \
    && chown -R steam:steam /data /config /home/steam

COPY --from=build /out/rustlife-server /usr/local/bin/rustlife-server
COPY --chmod=0755 deploy/entrypoint.sh /usr/local/bin/entrypoint.sh

USER steam
# NO_COLOR keeps the server's log free of terminal colour codes, which log
# viewers show as noise.
ENV HOME=/home/steam \
    RUST_LOG=info \
    NO_COLOR=1
# The game content, the server config, and SteamCMD with its state (in the
# steam user's home: SteamCMD keeps it in ~/Steam and ~/.steam).
VOLUME ["/data/hl2dm", "/config", "/home/steam"]
EXPOSE 27015/udp
ENTRYPOINT ["/usr/bin/tini", "-g", "--", "/usr/local/bin/entrypoint.sh"]
