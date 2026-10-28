# Deploying the dedicated server

rustlife's dedicated server, `rustlife-server`, ships as a container image
built from this repository (`deploy/Dockerfile`), with a
`docker-compose.yml` at the repository root that runs it. Any Docker host
works; this page walks through [Coolify](https://coolify.io), which builds
and runs the compose file straight from GitHub.

## What the container does

The image holds rustlife's server and Valve's SteamCMD installer, and no game
content. Each time the container starts, `deploy/entrypoint.sh`:

1. installs or updates the **Half-Life 2: Deathmatch Dedicated Server**
   (Steam app 232370) with SteamCMD, logged in anonymously, into the `hl2dm`
   volume (`/data/hl2dm`). It needs no Steam account and no game purchase;
2. checks that the install has what the server reads: `hl2mp/gameinfo.txt`,
   the `hl2mp/maps/dm_*.bsp` maps and the `hl2mp/hl2mp_pak` and
   `hl2/hl2_misc` VPKs (for the props' models, physics and `propdata`). The
   server needs no textures or sounds;
3. only if something is missing and `STEAM_USER` is set, downloads the game
   itself (app 320) with that account instead (see
   [Steam account fallback](#steam-account-fallback));
4. starts `rustlife-server` on that content with `/config/server.ron`,
   which it creates with commented defaults on the first start.

The server listens on **UDP 27015**. `docker stop` (and a Coolify redeploy or
stop) sends it SIGTERM: it tells connected players it is going away and
exits.

### First start

The first start downloads SteamCMD's update (about 200 MB) and then the
dedicated server, about **900 MB** in some 1,100 files. That takes a few
minutes, depending on the connection; nothing listens on the port until it
finishes. SteamCMD's downloads sometimes fail on a fresh install, so the
entrypoint retries them. Later starts only check for updates, which takes
seconds. Allow about **1.2 GB** of disk for the volumes.

## Deploying on Coolify

1. **Create the resource.** In your project, *+ New* → *Resource* →
   *Private Repository (with GitHub App)* (or *Public Repository* if the
   repository is public). Pick the GitHub App, the repository and the
   branch.
2. **Pick the build pack.** Set *Build Pack* to **Docker Compose**, *Base
   Directory* to `/` and *Docker Compose Location* to `/docker-compose.yml`.
   Continue.
3. **Leave the domain empty.** The server speaks UDP, which Coolify's HTTP
   proxy does not carry; players connect to the host's address directly.
4. **Check the port.** The compose file publishes `27015:27015/udp` on the
   host. Open UDP 27015 in the host's firewall (`ufw allow 27015/udp`, or
   the cloud provider's security group). To use another host port, change
   the left-hand number, e.g. `27016:27015/udp`; players then connect to
   that port.
5. **Check the volumes.** Coolify turns the three named volumes into
   persistent storage (listed under *Persistent Storage* after the first
   deploy), so the download survives redeploys:

   | Volume     | Mounted at    | Holds                                                      |
   |------------|---------------|------------------------------------------------------------|
   | `hl2dm`    | `/data/hl2dm` | the dedicated server install (900 MB)                      |
   | `config`   | `/config`     | `server.ron`                                               |
   | `steamcmd` | `/home/steam` | SteamCMD (200 MB) and, for the fallback, its Steam login   |

6. **Set environment variables** if needed (*Environment Variables*; Coolify
   lists the compose file's variables with their defaults). See
   [Environment](#environment). None is required.
7. **Deploy.** Follow the progress in the deployment's *Logs*: the image
   builds (a first build compiles the server, a few minutes), then the
   container downloads the game content. The server is up when it logs
   `serving` with the map name.

Players join from rustlife's menu (*Join a server*) or with
`rustlife --connect <host>:27015`.

## Configuring the server

Everything is in `/config/server.ron` in the `config` volume: the name, the
map rotation, the port, the player limit, the match, warmup, vote and
respawn times. The first start writes it with every option commented. After
editing it, **restart** the container (Coolify: *Restart*) for the changes to
apply.

The image has no text editor. Ways to edit the file:

- **Copy a file in** from the host running the container (find the
  container's name with `docker ps`; Coolify's ones end in the resource's
  id):

  ```sh
  docker cp <container>:/config/server.ron .   # fetch the current file
  # ... edit server.ron ...
  docker cp server.ron <container>:/config/server.ron
  docker restart <container>
  ```

- **Write it from a shell** in the container (Coolify: the resource's
  *Terminal*, or `docker exec -it <container> sh`):

  ```sh
  cat > /config/server.ron <<'EOF'
  (
      name: "My rustlife server",
      maps: ["dm_lockdown", "dm_overwatch", "dm_steamlab"],
      max_players: 12,
  )
  EOF
  ```

  Every field is optional; the ones left out keep their defaults. A typo in a
  field name stops the server with an error that names it.

- **Keep it in Coolify** with a file mount: replace the `config:/config`
  line in `docker-compose.yml` with a bind mount of a file Coolify stores,
  e.g.

  ```yaml
  volumes:
    - type: bind
      source: ./server.ron
      target: /config/server.ron
      content: |
        (name: "My rustlife server", max_players: 12)
  ```

  Coolify then shows the file under *Persistent Storage* and lets you edit it
  there. The server never rewrites an existing file.

If you change `port` in `server.ron`, change the right-hand number of the
port mapping to match.

## Environment

| Variable            | Default | Meaning                                                                                       |
|---------------------|---------|-----------------------------------------------------------------------------------------------|
| `RUST_LOG`          | `info`  | Log verbosity: `error`, `warn`, `info`, `debug`, `trace`, or per-module filters.              |
| `HL2DM_SKIP_UPDATE` | `0`     | `1`: start on the content already in the volume, without running SteamCMD.                    |
| `HL2DM_VALIDATE`    | `0`     | `1`: have SteamCMD check every installed file and repair the damaged ones (slow).              |
| `RUSTLIFE_ARGS`     | empty   | Extra `rustlife-server` arguments, e.g. `--map dm_lockdown` (first map) or `--dummies 2`.      |
| `STEAM_USER`        | empty   | Steam account owning Half-Life 2: Deathmatch, for the [fallback](#steam-account-fallback) only. |
| `STEAM_PASS`        | empty   | Its password (only needed until a login succeeds).                                            |
| `STEAM_GUARD`       | empty   | A current Steam Guard code for that login.                                                    |
| `STEAMCMD_RETRIES`  | `3`     | Attempts per SteamCMD run.                                                                    |

`rustlife-server --help` lists the server's arguments. Arguments given to the
container are passed on to it too: Compose's `command: ["--map",
"dm_overwatch"]`, or `docker run ... <image> --map dm_overwatch`.

## Steam account fallback

The anonymous dedicated-server download is all the server needs, so this is a
safety net: it is used only if that install turns out to lack one of the
files listed above, and the container then says which. In that case, and only
if `STEAM_USER` is set, the entrypoint downloads Half-Life 2: Deathmatch
(app 320, about 3 GB) with that account into `/data/hl2dm/app320` and serves
from it. The account must own the game.

Steam Guard makes an unattended first login awkward. Either:

- set `STEAM_USER`, `STEAM_PASS` and, just before starting, `STEAM_GUARD` to
  the code Steam e-mails (or the authenticator shows; it changes every 30
  seconds, so that is a race); or, more reliably,
- log in once interactively on the host, which keeps the login in the
  `steamcmd` volume. Once the container has started at least once (which
  puts SteamCMD in that volume), find the volume's and the image's names
  with `docker volume ls` and `docker images` (Coolify prefixes them with
  the resource's id), then run the following; SteamCMD asks for the password
  and the Steam Guard code:

  ```sh
  docker run --rm -it -v <steamcmd volume>:/home/steam \
      --entrypoint /home/steam/steamcmd/steamcmd.sh <image> \
      +login <user> +quit
  ```

After one successful login SteamCMD remembers it, so `STEAM_USER` alone
suffices: remove `STEAM_PASS` and `STEAM_GUARD`. The password is handed to
SteamCMD in a temporary file, not on its command line; it is quoted there,
so it must not contain a double quote (`"`). This path is untested (it needs
a real account); the anonymous one is what normal deployments use.

## Updating

- **rustlife:** push to the deployed branch and redeploy (Coolify can do it
  on every push through the GitHub App: *Automatic deployment*). The image
  rebuilds; the volumes, and so the game content and `server.ron`, stay.
- **Game content:** restarting the container runs SteamCMD's update check.
  Valve rarely updates Half-Life 2: Deathmatch, but when it changes a map,
  players with the new map are refused by a server with the old one (and the
  reverse) until both match.

Players must have the very same maps as the server (it compares their
checksums when they join). The dedicated server's maps are byte for byte
those of the game: all eight `hl2mp/maps/*.bsp` were compared in October
2026, along with `gameinfo.txt` and the `hl2mp_pak` and `hl2_misc` VPK
indexes. So players with an up-to-date Half-Life 2: Deathmatch can join.

## Legal

Half-Life 2: Deathmatch's content belongs to Valve and is covered by the Steam
Subscriber Agreement. Neither this repository nor the image contains any of
it: the container downloads the Dedicated Server, which Valve distributes
free of charge for running servers, onto your own machine. Do not publish an
image or a volume with the content in it. Players need their own copy of
Half-Life 2: Deathmatch.
