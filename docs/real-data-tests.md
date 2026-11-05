# Real-data tests

```sh
cargo test --workspace
```

With no configuration, this runs the hand-built tests, the doc test and the synthetic EBX fixtures in `tests/fixtures`. The tests that read your own game files skip, and each prints one line naming the key it needs.

To run them against your own copy of the game, write a TOML file and point the `GAME_DATA` environment variable at it. The file names installs, keys and tools. Its format belongs to [primase-testdata](https://github.com/Kalmarv/primase-testdata), and this crate reads these keys:

```toml
[installs]
skate = 'D:/games/skate'                 # the install: its Data folder holds layout.toc, the TOCs and the CAS archives

[tools]
oodle9 = 'D:/games/skate/oo2core_9_win64.dll'   # the game's own Oodle library (oodle5 and oodle6 are for the CAS decoder test)
reskate_cli = 'D:/tools/ReSkateStudio/reskate_cli.exe'   # optional: ReSkate Studio's CLI, the reference EBX reader and writer

[oracles]
root = 'D:/oracles'    # <root>/corpus/skate/ebx: EBX documents extracted from the game, as stored, under their asset paths
                       # <root>/corpus/skate/cas: individual `.casblock` frames
                       # <root>/baseline/skate/fbmod: `.fbmod` files to round-trip

[run]
require = 'gate'       # optional: an input marked `gate` fails its test instead of skipping
```

[tests/inputs/README.md](../tests/inputs/README.md) lists every input, what it is and where it comes from. It is generated from `tests/inputs/mod.rs`, and `tests/real_data_inputs.rs` fails when the two differ. The tests need only read access to your files.

The doc test is checked with `cargo test -p frostbite-formats --doc`, and the example programs are built with `cargo build -p frostbite-formats --examples`.
