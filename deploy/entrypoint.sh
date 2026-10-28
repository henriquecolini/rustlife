#!/bin/sh
# Container entrypoint of the rustlife dedicated server.
#
# 1. Installs or updates the Half-Life 2: Deathmatch Dedicated Server (Steam
#    app 232370, anonymous login) with SteamCMD into $HL2DM_INSTALL_DIR.
# 2. Checks that the install has the content rustlife's server reads.
# 3. If it does not and Steam credentials are set, installs the game itself
#    (app 320, which needs an account that owns it) and uses that instead.
# 4. Replaces itself with rustlife-server, so that the server receives
#    `docker stop`'s SIGTERM directly (through tini) and shuts down cleanly.
#
# Arguments are passed on to rustlife-server, e.g. `--map dm_lockdown`.
# Environment (see docs/deploy.md):
#   HL2DM_INSTALL_DIR  where the game content is installed (/data/hl2dm)
#   SERVER_CONFIG      the server's config file (/config/server.ron)
#   HL2DM_SKIP_UPDATE  1: do not run SteamCMD, use the install as it is
#   HL2DM_VALIDATE     1: have SteamCMD verify every installed file
#   STEAMCMD_RETRIES   attempts per SteamCMD run (3)
#   STEAM_USER, STEAM_PASS, STEAM_GUARD
#                      an account owning Half-Life 2: Deathmatch, used only if
#                      the dedicated server lacks needed content
#   RUSTLIFE_ARGS      extra rustlife-server arguments, split on spaces

set -eu

INSTALL_DIR=${HL2DM_INSTALL_DIR:-/data/hl2dm}
# The game (app 320), installed only by the fallback, next to the dedicated
# server so that it lives on the same volume.
GAME_INSTALL_DIR=$INSTALL_DIR/app320
SERVER_CONFIG=${SERVER_CONFIG:-/config/server.ron}
STEAMCMD_DIR=$HOME/steamcmd
STEAMCMD=$STEAMCMD_DIR/steamcmd.sh
RETRIES=${STEAMCMD_RETRIES:-3}

DEDICATED_SERVER_APP=232370
GAME_APP=320

log() {
    printf '[entrypoint] %s\n' "$*"
}

die() {
    printf '[entrypoint] error: %s\n' "$*" >&2
    exit 1
}

# Runs SteamCMD with the given arguments until it succeeds, at most $RETRIES
# times: on a fresh volume its first run updates SteamCMD itself and often
# fails the download that follows.
steamcmd() {
    attempt=1
    while :; do
        if "$STEAMCMD" "$@"; then
            return 0
        fi
        if [ "$attempt" -ge "$RETRIES" ]; then
            return 1
        fi
        attempt=$((attempt + 1))
        log "SteamCMD failed; retrying (attempt $attempt of $RETRIES)"
        sleep 5
    done
}

# Prints the SteamCMD command that installs or updates app $1, verifying
# every file if HL2DM_VALIDATE=1. On SteamCMD's command line it takes a
# leading `+`; in a script it does not.
app_update() {
    if [ "${HL2DM_VALIDATE:-0}" = 1 ]; then
        printf 'app_update %s validate' "$1"
    else
        printf 'app_update %s' "$1"
    fi
}

# Lists, one per line, what the install at $1 lacks of the content
# rustlife-server reads: gameinfo.txt (which drives mounting), the
# deathmatch maps, and the VPKs holding the models' physics and propdata.
missing_content() {
    dir=$1
    [ -f "$dir/hl2mp/gameinfo.txt" ] || echo "hl2mp/gameinfo.txt"
    if ! ls "$dir"/hl2mp/maps/dm_*.bsp >/dev/null 2>&1; then
        echo "hl2mp/maps/dm_*.bsp (the deathmatch maps)"
    fi
    [ -f "$dir/hl2mp/hl2mp_pak_dir.vpk" ] || echo "hl2mp/hl2mp_pak_dir.vpk"
    [ -f "$dir/hl2/hl2_misc_dir.vpk" ] || echo "hl2/hl2_misc_dir.vpk"
}

# Installs the dedicated server anonymously.
install_dedicated_server() {
    log "installing or updating the HL2DM Dedicated Server (app $DEDICATED_SERVER_APP) into $INSTALL_DIR"
    log "(a first install downloads about 900 MB, which can take a few minutes)"
    # shellcheck disable=SC2046 # app_update's words are meant to be split.
    steamcmd +force_install_dir "$INSTALL_DIR" +login anonymous \
        +$(app_update "$DEDICATED_SERVER_APP") +quit
}

# Installs the game itself with the account in $STEAM_USER. The login is
# passed in a script file rather than on the command line, so that the
# password does not show in the process list.
install_game() {
    log "installing or updating Half-Life 2: Deathmatch (app $GAME_APP) as $STEAM_USER into $GAME_INSTALL_DIR"
    script=$(mktemp)
    # shellcheck disable=SC2064 # The path is fixed now on purpose.
    trap "rm -f '$script'" EXIT
    chmod 600 "$script"
    {
        echo "force_install_dir \"$GAME_INSTALL_DIR\""
        # Once a login succeeds, SteamCMD remembers it in its volume,
        # and the user name alone suffices.
        printf 'login "%s"' "$STEAM_USER"
        [ -n "${STEAM_PASS:-}" ] && printf ' "%s"' "$STEAM_PASS"
        [ -n "${STEAM_GUARD:-}" ] && printf ' "%s"' "$STEAM_GUARD"
        echo
        app_update "$GAME_APP"
        echo
        echo quit
    } >"$script"
    steamcmd +runscript "$script"
    rm -f "$script"
    trap - EXIT
}

# Puts SteamCMD's bootstrapper, shipped in the image, where SteamCMD can
# update itself: in the home volume, so the update is kept.
install_steamcmd() {
    if [ ! -x "$STEAMCMD" ]; then
        log "installing SteamCMD into $STEAMCMD_DIR"
        mkdir -p "$STEAMCMD_DIR"
        cp -R /opt/steamcmd/. "$STEAMCMD_DIR"
    fi
}

# --- Content ----------------------------------------------------------------
game_dir=$INSTALL_DIR
if [ "${HL2DM_SKIP_UPDATE:-0}" != 1 ] || [ -n "${STEAM_USER:-}" ]; then
    install_steamcmd
fi
if [ "${HL2DM_SKIP_UPDATE:-0}" = 1 ]; then
    log "HL2DM_SKIP_UPDATE=1: not running SteamCMD"
elif ! install_dedicated_server; then
    log "SteamCMD could not install app $DEDICATED_SERVER_APP; checking what is already there"
fi

missing=$(missing_content "$INSTALL_DIR")
if [ -n "$missing" ]; then
    log "the dedicated server install in $INSTALL_DIR lacks:"
    printf '%s\n' "$missing" | sed 's/^/[entrypoint]   /'
    [ -n "${STEAM_USER:-}" ] \
        || die "set STEAM_USER (and STEAM_PASS, STEAM_GUARD) to an account owning Half-Life 2: Deathmatch to download the game instead"
    game_dir=$GAME_INSTALL_DIR
    if [ "${HL2DM_SKIP_UPDATE:-0}" != 1 ]; then
        install_game || log "SteamCMD could not install app $GAME_APP"
    fi
    missing=$(missing_content "$game_dir")
    if [ -n "$missing" ]; then
        printf '%s\n' "$missing" | sed 's/^/[entrypoint]   missing: /' >&2
        die "the game install in $game_dir lacks the content above"
    fi
fi
log "game content found in $game_dir"

# --- Server -----------------------------------------------------------------
# rustlife-server writes a commented default config on its first run.
[ -f "$SERVER_CONFIG" ] || log "creating the default server config at $SERVER_CONFIG"

# shellcheck disable=SC2086 # RUSTLIFE_ARGS is a list of words on purpose.
exec /usr/local/bin/rustlife-server \
    --game-dir "$game_dir" \
    --server-config "$SERVER_CONFIG" \
    ${RUSTLIFE_ARGS:-} "$@"
