#!/bin/sh
# Container entrypoint of the rustlife dedicated server.
#
# 1. Installs or updates the Half-Life 2: Deathmatch Dedicated Server (Steam
#    app 232370, anonymous login) with DepotDownloader into
#    $HL2DM_INSTALL_DIR.
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
#   HL2DM_SKIP_UPDATE  1: do not download, use the install as it is
#   HL2DM_VALIDATE     1: have DepotDownloader verify every installed file
#   DOWNLOAD_RETRIES   attempts per DepotDownloader run (3)
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
DEPOTDOWNLOADER=/opt/depotdownloader/DepotDownloader
RETRIES=${DOWNLOAD_RETRIES:-3}

DEDICATED_SERVER_APP=232370
GAME_APP=320

log() {
    printf '[entrypoint] %s\n' "$*"
}

die() {
    printf '[entrypoint] error: %s\n' "$*" >&2
    exit 1
}

# Runs DepotDownloader with the given arguments until it succeeds, at most
# $RETRIES times: Steam's content servers sometimes drop a download.
# DepotDownloader resumes where the failed attempt stopped.
depotdownloader() {
    attempt=1
    while :; do
        if "$DEPOTDOWNLOADER" "$@"; then
            return 0
        fi
        if [ "$attempt" -ge "$RETRIES" ]; then
            return 1
        fi
        attempt=$((attempt + 1))
        log "DepotDownloader failed; retrying (attempt $attempt of $RETRIES)"
        sleep 5
    done
}

# Prints DepotDownloader's options for installing or updating app $1: its
# Linux depots (whatever the host), verifying every file if HL2DM_VALIDATE=1.
app_update() {
    printf -- '-app %s -os linux' "$1"
    if [ "${HL2DM_VALIDATE:-0}" = 1 ]; then
        printf -- ' -validate'
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
    # Nothing to type at an anonymous login: no standard input, so that an
    # unexpected prompt fails the attempt rather than waiting forever.
    # shellcheck disable=SC2046 # app_update's words are meant to be split.
    depotdownloader $(app_update "$DEDICATED_SERVER_APP") -dir "$INSTALL_DIR" </dev/null
}

# Installs the game itself with the account in $STEAM_USER. With
# -remember-password, DepotDownloader keeps the login in the home volume
# after it first succeeds, and the user name alone suffices from then on. A
# Steam Guard code is read from standard input, so $STEAM_GUARD is fed to it
# there (an empty line if unset, so that it does not wait for one).
install_game() {
    log "installing or updating Half-Life 2: Deathmatch (app $GAME_APP) as $STEAM_USER into $GAME_INSTALL_DIR"
    set -- -username "$STEAM_USER" -remember-password -no-mobile
    [ -n "${STEAM_PASS:-}" ] && set -- "$@" -password "$STEAM_PASS"
    # shellcheck disable=SC2046 # app_update's words are meant to be split.
    printf '%s\n' "${STEAM_GUARD:-}" \
        | depotdownloader $(app_update "$GAME_APP") -dir "$GAME_INSTALL_DIR" "$@"
}

# --- Content ----------------------------------------------------------------
game_dir=$INSTALL_DIR
if [ "${HL2DM_SKIP_UPDATE:-0}" = 1 ]; then
    log "HL2DM_SKIP_UPDATE=1: not downloading"
elif ! install_dedicated_server; then
    log "DepotDownloader could not install app $DEDICATED_SERVER_APP; checking what is already there"
fi

missing=$(missing_content "$INSTALL_DIR")
if [ -n "$missing" ]; then
    log "the dedicated server install in $INSTALL_DIR lacks:"
    printf '%s\n' "$missing" | sed 's/^/[entrypoint]   /'
    [ -n "${STEAM_USER:-}" ] \
        || die "set STEAM_USER (and STEAM_PASS, STEAM_GUARD) to an account owning Half-Life 2: Deathmatch to download the game instead"
    game_dir=$GAME_INSTALL_DIR
    if [ "${HL2DM_SKIP_UPDATE:-0}" != 1 ]; then
        install_game || log "DepotDownloader could not install app $GAME_APP"
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
