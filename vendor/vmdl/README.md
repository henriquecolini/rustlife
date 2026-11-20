# vmdl (vendored)

A copy of [vmdl](https://github.com/icewind1991/vmdl) 0.2.0 (MIT licence, see
Cargo.toml), used through `[patch.crates-io]` in the workspace manifest.

## What differs from the published crate

One thing. `src/mdl/raw/animation.rs` had

```rust
todo!("read animation from animation block");
```

for animations stored in an external animation block. It panicked on 11
stock Half-Life 2 models (found by `source_formats`'s `real_panics` test). A
desktop build contains such panics with `catch_unwind`, but a browser build
(wasm) aborts on any panic, so reading one of those models would end the game.
The copy returns `ModelError::Unsupported` there instead, which callers
already treat as "this model cannot be read", exactly as the contained panic
made them. `src/error.rs` gains that variant.

Drop this copy when upstream stops panicking there.
