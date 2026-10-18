# KeyValues

Implemented in `crates/source_fs/src/keyvalues.rs`. Used by `gameinfo.txt`,
Steam `.vdf`/`.acf`, materials (`.vmt`), soundscripts and map entity data.

```text
"Root"
{
    "key"   "value"          // comment
    bare_key bare_value
    "nested" { "k" "v" }
    "pc_only" "1" [$WIN32]   // platform conditional
}
```

Behaviour matched from the engine:

* keys are case-insensitive and may repeat; lookups return the first match;
* escapes are off by default (content paths use backslashes);
* conditionals are evaluated against the PC build's symbols (`$WIN32`,
  `$WINDOWS`, `$PC`) regardless of host OS;
* unclosed blocks at EOF are tolerated; a stray `}` is an error;
* `#base`/`#include` are recorded, not resolved.
