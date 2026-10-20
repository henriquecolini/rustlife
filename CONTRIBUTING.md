# Contributing

## Ground rules

* **No game content in the repository, ever.** Tests generate the content they
  need (see `source_fs::testing`). `cargo xtask check-assets` enforces this by
  extension and by file signature.
* Every commit builds, is formatted, passes clippy with warnings as errors, and
  passes all tests: run `cargo xtask ci` before committing.
* Commit messages follow [Conventional Commits](https://www.conventionalcommits.org):
  `feat(source_fs): mount workshop VPKs`, `fix(movement): clamp step height`.

## Code style

* Library crates return typed errors (`thiserror`); only binaries use `anyhow`.
  Malformed content must never panic: report an error and let the caller fall
  back to a placeholder.
* `unwrap`/`expect`/`panic!` are denied outside tests by clippy configuration.
* No `unsafe` (`#![forbid(unsafe_code)]` in every crate).
* Every public item has a doc comment. Comments explain *why* — format quirks,
  engine behaviour being matched, netcode reasoning — not *what*.
* Keep pure logic separate from Bevy systems so it can be unit-tested without
  an `App`.
* Gameplay numbers belong in `data/*.ron`, not in code (see
  [docs/data-driven.md](docs/data-driven.md)).

## Tests

* Unit tests live next to the code (`#[cfg(test)] mod tests`).
* Parsers get property tests (`proptest`) asserting they never panic on
  arbitrary input.
* Tests that need a real install are `#[ignore]`d and live in `tests/`;
  run them with `cargo xtask content-tests`.
