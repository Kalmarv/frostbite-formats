# frostbite-formats

[![crates.io: frostbite-formats](https://img.shields.io/crates/v/frostbite-formats?label=frostbite-formats)](https://crates.io/crates/frostbite-formats) [![docs.rs](https://img.shields.io/docsrs/frostbite-formats)](https://docs.rs/frostbite-formats) [![CI](https://img.shields.io/github/actions/workflow/status/Kalmarv/frostbite-formats/ci.yml?branch=main&label=CI)](https://github.com/Kalmarv/frostbite-formats/actions/workflows/ci.yml)

A Rust library that reads and writes the Frostbite file formats of the game skate. (2025). It covers the installed game's data folder and the `.fbmod` mods that [ReSkate Studio](https://github.com/Dingo-Shenanigans/ReSkate) writes.

## Features

- Reads superbundle TOCs (Huffman-coded names in the base game, plain names in Patch TOCs) and writes Patch TOCs
- Merges a Patch TOC with its base game TOC into the TOC the game sees
- Reads and writes bundle file tables, binary bundle manifests and their chunk metadata, the native DB format and `layout.toc`
- Decodes CAS archive blocks (raw, zlib, LZ4 and Oodle) and writes Kraken-compressed or stored blocks
- Opens a game `Data` folder, or a mod folder over it, and lists the EBX documents, resources and chunks of each bundle
- Reads EBX documents (REFL and RFL2 reflection) into typed values, writes them back, and copies values and GUIDs between documents
- Reads and writes ReSkate Studio `.fbmod` files (Frosty mod version 6), payloads included
- Reads the shader database: block depots, parameter keys, program lookups, permutation selectors, permutations with their pipeline state, binding sets, input layouts and DXBC containers
- Reads and writes MeshSets, and builds a rigid MeshSet and its geometry chunk from triangles
- Reads and writes physics resources, and encodes triangle meshes as collision shapes
- Reads skeleton assets, and reads and writes 180-byte texture headers
- Writing back what was read gives the same bytes
- Bounds-checks every read: a bad file gives an `Error` naming what was read, the offset and what was expected
- Refuses values it does not recognise instead of guessing, and keeps unknown fields as stored
- Touches the filesystem only in the `folder` module, and ships no game data or Oodle library

## Getting Started

### Requirements

- Rust 1.99 or later (edition 2024).
- A copy of skate. to read. The crate holds no game data.
- To decode Oodle-compressed CAS blocks, an Oodle handle from [primase-foundations](https://github.com/Kalmarv/primase-foundations): on Windows load the game's own `oo2core_9_win64.dll`; elsewhere `Oodle::portable()` decodes without it.

### Installation

```sh
cargo add frostbite-formats
cargo add primase-foundations --features compress
```

`primase-foundations` is only needed for the Oodle handle.

### Usage

List the bundles of one TOC in the game's `Data` folder with how many EBX documents, resources and chunks each holds:

```rust
use frostbite_formats::bundle::Kind;
use frostbite_formats::folder::{Folder, Source};
use primase_foundations::compress::oodle::{Oodle, Version};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dll = PathBuf::from("D:/games/skate/oo2core_9_win64.dll");
    // SAFETY: loading the game's own Oodle library runs its initialisation code.
    let oodle = unsafe { Oodle::load(dll, Version::V9) }?;
    let folder = Folder::open("D:/games/skate/Data")?;
    let source = Source { folder: &folder, base: Some(&folder), oodle: Some(&oodle) };

    let listing = source.listing("Win32/globals.toc")?;
    for bundle in &listing.bundles {
        let count = |kind| bundle.entries().filter(|e| e.kind == kind).count();
        println!(
            "{}: {} EBX, {} resources, {} chunks",
            bundle.name,
            count(Kind::Ebx),
            count(Kind::Res),
            count(Kind::Chunk)
        );
    }
    Ok(())
}
```

The same program ships as an example that takes its paths from the command line:

```sh
cargo run --example list_bundles -- <game>/Data Win32/globals.toc <game>/oo2core_9_win64.dll
```

Other entry points:

- `Source::ebx(toc, name)` reads one EBX document; `ebx::read` and `ebx::write` work on bytes, and `ebx::carry::Carrier` copies values between documents.
- `Source::merged_toc(rel)` gives a mod folder's TOC merged with the base game's.
- `fbmod::read` and `fbmod::write` handle `.fbmod` files.
- `meshset::read_resource` and `meshset::write`, `physics::read` and `physics::write`, and `texture::read` and `texture::write` handle the resources.

The [API documentation](https://docs.rs/frostbite-formats) covers every module, and [docs/modules.md](docs/modules.md) maps modules to formats. [docs/guarantees.md](docs/guarantees.md) lists what the crate guarantees and what it does not do.

`cargo test --workspace` runs the hand-built tests, the doc test and the EBX fixtures without any game files. The tests that read your own copy of the game are configured through the `GAME_DATA` file described in [docs/real-data-tests.md](docs/real-data-tests.md), and skip when it is not set.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the checks CI runs and how to report a file the crate cannot read. Licensed under GPL-3.0-or-later. The `ebx` module is a port of ReSkate's EBX reader, writer and merge.
