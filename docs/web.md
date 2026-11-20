# The web build

rustlife also runs in a browser (WebGPU in current Safari, Chrome and Edge),
including on a phone: a page that loads the game as WebAssembly, copies the
player's **own** Half-Life 2: Deathmatch files into the browser's private
storage once, and plays from there. It is installed to the Home Screen like an
app, with no store and no signing.

The page contains **no game content**, like the desktop game. The files stay
on the player's device; nothing is uploaded.

## How it fits together

```
 page (web/index.html, app.js)         the menu: import, choose a map, play
   │
 rl_web  (wasm-bindgen exports)        status, chooseFolder, importGame, play, …
   │  ├─ import      what to copy, from data (crates/rl_web/data/import.ron)
   │  ├─ manifest    what was copied (text, resumable)
   │  └─ browser     OPFS storage + folder picker (js/storage.js)
   │
 source_fs::remote                     mounts the imported install: the same
   │                                   gameinfo.txt search paths and VPK format
   │                                   as desktop, read through `ByteSource`s
 rl_client / rl_level / rl_server      the game, unchanged, in one thread
```

Everything below `rl_web` is the same code as the desktop game. The web build
adds seams, never forks: `ByteSource` (where bytes come from), `Mount::read_async`
and `make_resident` (content that arrives asynchronously), `MapLoader::with_pending`
(a map that is still arriving).

## Content: the central difference

A browser has no filesystem, and every read of the files it holds is
asynchronous. The desktop game reads files synchronously in a few places (the
sound scripts at startup, a map's surface materials, the physics of a level's
models). So:

* **Import** (once). The player picks their install folder. `rl_web::import`
  plans which files to copy (textures, models, sounds, the mod folder; not
  binaries or the optional voice-over), and `run_import` copies them into the
  browser's origin private filesystem (OPFS) through a worker, recording each
  finished file in a manifest so an interrupted import resumes.
* **Mount** (every visit). `mount_remote_game` mounts the imported files in the
  order `gameinfo.txt` lists, as `RemoteVpk` and `RemoteFiles` mounts. Reads of
  a file are ranges fetched on demand (`File.slice`), never the whole archive.
* **Asynchronous reads.** Bevy's asset loaders already await, so textures,
  models and sounds load through `Vfs::read_async`.
* **Synchronous reads.** `Vfs::read` on a file that is not in memory fails with
  `Error::NotResident` and records the path (`Vfs::take_misses`). Before they
  are needed, `rl_client::prepare` makes the files resident: `startup` once,
  `map` before each map. `rl_level::load` reports what a level was missing
  instead of building it without; `load_async` fetches those files and tries
  again until none are missing.
* **Memory.** Resident files live in a byte-budgeted LRU per mount; files the
  client always needs (scripts, particles) are pinned.

## Differences from desktop

* **Single thread.** Bevy's task pools run on the page's thread. Loading is
  asynchronous, and frame work has a budget; real threads would need
  cross-origin isolation, which static hosting cannot set.
* **No panics.** WebAssembly aborts on a panic instead of unwinding, so the
  `catch_unwind` guards around the parsing crates do nothing there. The one
  panic real content triggered (11 stock models, an unimplemented animation
  block in `vmdl`) is fixed at the source in `vendor/vmdl`; the
  `real_panics` test (`RUSTLIFE_TEST_GAME_DIR=… cargo test -p source_formats
  --test real_panics -- --ignored`) sweeps the whole install for others.
* **Textures.** iPhone GPUs cannot sample BC-compressed textures, so the
  loader decodes to RGBA8 there (with the texture-quality setting's mip
  dropping to bound memory).
* **Shaders** must pass the browser's WGSL validator, which rejects
  derivatives (`textureSample`) in non-uniform control flow.
* **No UDP.** Single-player (an in-process server) works as on desktop;
  joining a remote server is not available in the browser yet.

## Building and testing

```sh
rustup target add wasm32-unknown-unknown
cargo binstall wasm-bindgen-cli@<the version in Cargo.lock>

cargo xtask web-build      # dist/web: wasm module + page
cargo xtask web-serve      # http://localhost:8080/
cargo xtask web-test       # browser tests in headless Chrome (needs chromedriver)
```

`cargo xtask ci` type-checks and lints the web-capable crates for
`wasm32-unknown-unknown` and, when `wasm-bindgen-test-runner` and a matching
`chromedriver` are installed, runs the browser tests.

Localhost is a secure context, so WebGPU and OPFS work in development; a phone
needs HTTPS (GitHub Pages provides it).
