# frostbite-formats

A Rust library that reads and writes the Frostbite container and resource formats of the game *skate.* (2025): bytes in, plain records out, and the same records back to bytes.

It reads the game's data folder (superbundle TOCs, `layout.toc`, bundles, CAS archives, EBX documents, shader, material, MeshSet, physics, skeleton and texture resources) and the `.fbmod` mods that [ReSkate Studio](https://github.com/Dingo-Shenanigans/ReSkate) writes. It was written against the skate. build installed from the game and the files ReSkate Studio 20261012 writes; other Frostbite titles are not assumed to match. It is an independent reader of the stored formats with no code from Frosty or FrostyToolsuite, except that the EBX module is a port of ReSkate's EBX reader, writer and merge (GPL-3.0-or-later).

## What it reads and writes

| module | format |
| --- | --- |
| `toc`, `bundle`, `db` | superbundle TOCs (Huffman-coded names in the base game, plain names in Patch TOCs, the perfect-hash tables), a bundle's file table and binary manifest, the native DB behind `layout.toc` |
| `cas`, `folder`, `listing`, `scope` | CAS archives (raw, zlib and Oodle blocks), a data folder with its archives, the bundles and entries a TOC lists, and ids and names looked up across several bundles |
| `ebx` | EBX documents (RIFF `EBX`/`EBXS`, REFL or RFL2 reflection) read into typed values, written again, and carried between documents |
| `fbmod` | `.fbmod` mods in the layout ReSkate Studio writes (Frosty mod version 6), payloads included |
| `depot`, `lookup`, `selector`, `permutation`, `binding`, `inputlayout`, `bytecode` | the shader database: block depots, program and texture lookup tables, the permutation selector, permutations with their fixed-function state, binding sets, input layouts, and the DXBC/DXIL containers of compiled bytecode |
| `meshset`, `rigid`, `tangent`, `physics`, `skeleton`, `texture` | MeshSets (LODs, sections, vertex declarations), rigid meshes built from triangles, the packed tangent frame, physics resources (with an encoder for triangle meshes), skeleton assets, 180-byte texture headers |

Oodle-compressed CAS blocks are decoded through a handle to the game's own Oodle library (`oo2core_9_win64.dll`), which you load and pass in as a [`primase_foundations::compress::oodle::Oodle`](https://github.com/Kalmarv/primase-foundations) (`Oodle::load`; off Windows, `Oodle::portable()` decodes without the library). The library is not shipped here.

## Usage

```toml
[dependencies]
frostbite-formats = { git = "https://github.com/Kalmarv/frostbite-formats" }
```

Build a one-bundle superbundle TOC, read it back, and check that the bundle and chunk lookups resolve (this is the example in the crate documentation, run as a doc test):

```rust
use frostbite_formats::bundle;
use frostbite_formats::toc::{self, Loc, TocChunk};

fn main() -> Result<(), frostbite_formats::Error> {
    let archive = Loc { patch: true, chunk: 0, archive: 1 };
    // a bundle whose region lists one file: 16 bytes at offset 0 of the archive
    let region = bundle::write_region(&[(archive, 0, 16)], &[])?;
    let chunk = TocChunk { guid: [7; 16], file: Some((archive, 16, 64)) };

    let bytes = toc::write(&[("win32/levels/demo".to_string(), region)], &[chunk], 0)?;

    let parsed = toc::read(&bytes)?;
    toc::verify(&parsed)?;
    assert_eq!(parsed.bundles[0].0, "win32/levels/demo");
    let (files, _inline_manifest) = bundle::read_region(&parsed.bundles[0].1)?;
    assert_eq!(files, [(archive, 0, 16)]);
    assert_eq!(parsed.chunks[0].file, Some((archive, 16, 64)));

    // a corrupt file is refused with where and why
    let err = toc::read(&bytes[..100]).unwrap_err();
    assert!(err.to_string().contains("toc at 0x0"));
    Ok(())
}
```

To read a real data folder, open it with `folder::Folder::open`, wrap it in a `folder::Source` (with the game's Oodle library), and list a TOC's bundles with `Source::listing`. `examples/list_bundles.rs` does that:

```text
cargo run --example list_bundles -- <game>/Data Win32/globals.toc <game>/oo2core_9_win64.dll
```

## What it guarantees

- **Checked reads.** Every read goes through a bounds-checked reader. A file that is truncated or does not hold to the format returns an `Error` that names what was being read, the offset and what was expected (`toc at 0x0: envelope is invalid`). Counts read from a file are checked against the bytes that remain before a table is reserved for them (`tests/containers.rs`, `tests/fbmod.rs`), and nesting in the native DB is bounded (`tests/robust.rs`).
- **Strict readers.** They walk to the exact end of what they read and refuse a value they do not know instead of guessing. Fields whose meaning is not known are kept as stored.
- **Writers are the readers' counterparts.** What is read and written again comes back byte for byte (TOCs are written only in the Patch layout, with plain names). The tests check this on hand-built input for Patch TOCs, bundle regions and manifests, the native DB, shader depots, program lookups and `.fbmod` files, and, when your game files are configured, on the game's own MeshSets, physics resources and texture headers, and on `.fbmod` files written by ReSkate Studio. An EBX document keeps every field's value through a rewrite, and comes back byte for byte when it already has the writer's layout.
- **No game data, no hidden I/O.** The crate holds no game data and no keys, and touches files only in `folder`, which opens the files of a folder you name.
- **Documented API.** `#![deny(missing_docs)]` is set, so every public item is documented.

## What it does not do

- It does not ship, find or download any game file, and it does not ship Oodle.
- It does not decode pixels, vertices or shader code: it reads the headers, tables and records around them (a texture header, a MeshSet's sections and where their vertices lie in the geometry chunk, the DXBC containers of a bytecode resource).
- It does not read other Frostbite games, and it is not compatible with Frosty's own project or mod formats beyond the `.fbmod` layout named above.
- It does not promise to be panic-free on arbitrary bytes, but it is built to be. Library code denies unchecked indexing and slicing, unchecked integer arithmetic and narrowing casts (`clippy::indexing_slicing`, `arithmetic_side_effects` and `cast_possible_truncation` in `src/lib.rs`), so a stored offset, count or size is read through a checked read and a value that does not fit is an `Error`. A few items allow one of these lints, each with its reason in the source: the tangent codec (indexing into three-element arrays, quantising casts), two casts in the EBX encoder (taken after the value is range-checked), and the narrowing of computed floats (and one clamped bin index) in `physics` and `rigid`. The overflows found before the lints, and the allocation and nesting bounds, are pinned in `tests/robust.rs`. A byte-mutation sweep of hand-built TOCs, bundle regions, binding sets, physics resources, native DBs, MeshSets, `.fbmod` files, texture headers and EBX documents found no panic; that is a sample, not a proof.
- Whole files are read into memory; there is no streaming reader.

## Tests

```text
cargo test --workspace
```

With no configuration this runs the hand-built tests, the doc test and the synthetic EBX fixtures in `tests/fixtures`; the tests that read your own game files skip, each with one line naming the key it needs.

To run them against your own copy of the game, write a TOML file and point `GAME_DATA` at it. It names installs, keys and tools; the format belongs to [primase-testdata](https://github.com/Kalmarv/primase-testdata), and this crate reads these keys:

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

[tests/inputs/README.md](tests/inputs/README.md) lists every input, what it is and where it comes from; it is generated from `tests/inputs/mod.rs`, and `tests/real_data_inputs.rs` fails when the two differ. Only read-only access to your files is needed.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md), including how to report a file this crate cannot read.

## License

GPL-3.0-or-later, see [LICENSE](LICENSE). Used by [primase](https://github.com/Kalmarv/primase), a game-asset conversion library.
