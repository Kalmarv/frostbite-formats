# Contributing

## Build and test

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

The toolchain is pinned in `rust-toolchain.toml`. Without a `GAME_DATA` file the tests that read game files skip, each printing one line that names the key it needs; the rest, including the doc test and the example in the README, run as they are. To run the real-data tests against your own copy of the game, see "Tests" in the [README](README.md) and [tests/inputs/README.md](tests/inputs/README.md).

`tests/inputs/README.md` is generated from `tests/inputs/mod.rs`: when you change what a test reads, change the declaration there, and the table in the README until `cargo test --test real_data_inputs` passes (the failure prints the table it expects).

## Reporting a file this crate cannot read

Open an issue with the "Unreadable file" template. It asks for:

- the version of this crate (or the commit) and your platform;
- the exact error message, which names the format, the offset and what was expected (for example `meshset at 0xb4: 9 LODs, ...`);
- what the file is: its path inside the game's `Data` folder or mod, its size, and how you got it (read from the install, extracted by a tool, written by a mod tool);
- the game build, if you know it.

Do not attach game files, archives, keys or anything extracted from a game: they are not ours to redistribute, and the report does not need them. Describe the file instead. If the offset in the message points inside the file, the first bytes there in hex (a few dozen at most) are usually enough.

## Changing the code

- Keep the public items documented and the docs true of the code: a claim about a format names how it was checked (which files, how many).
- A reader refuses what it does not understand, with an error that names what was being read and the offset; it does not guess. A writer is the counterpart of its reader.
- Add a test next to the behaviour you change. A test that needs game files declares its input in `tests/inputs/mod.rs` and skips when it is not configured; it does not read paths of its own.
