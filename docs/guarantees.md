# Guarantees and limits

## What it guarantees

- **Checked reads.** Every read goes through a bounds-checked reader. A file that is truncated or does not hold to the format returns an `Error` that names what was being read, the offset and what was expected (`toc at 0x0: envelope is invalid`). Counts read from a file are checked against the bytes that remain before a table is reserved for them (`tests/containers.rs`, `tests/fbmod.rs`), and nesting in the native DB is bounded (`tests/robust.rs`).
- **Strict readers.** They walk to the exact end of what they read and refuse a value they do not know instead of guessing. Fields whose meaning is not known are kept as stored.
- **Writers are the readers' counterparts.** Something read and written again comes back byte for byte (TOCs are written only in the Patch layout, with plain names). The tests check this on hand-built input for Patch TOCs, bundle regions and manifests, the native DB, shader depots, program lookups and `.fbmod` files. When your game files are configured, they also check it on the game's own MeshSets, physics resources and texture headers, and on `.fbmod` files written by ReSkate Studio. An EBX document keeps every field's value through a rewrite, and comes back byte for byte when it already has the writer's layout.
- **No game data, no hidden I/O.** The crate holds no game data and no keys, and touches files only in `folder`, which opens the files of a folder you name.
- **Documented API.** `#![deny(missing_docs)]` is set, so every public item is documented.

## What it does not do

- It does not ship, find or download any game file, and it does not ship Oodle.
- It does not decode pixels, vertices or shader code. It reads the headers, tables and records around them (a texture header, a MeshSet's sections and where their vertices lie in the geometry chunk, the DXBC containers of a bytecode resource).
- It does not read other Frostbite games. It is not compatible with Frosty's own project or mod formats beyond the `.fbmod` layout named above.
- It does not decode Zstandard CAS blocks (codec 0x0f), which may need a dictionary from the initfs. No stock or mod block uses that codec.
- It refuses physics resources that hold the two tables not read yet (24- and 44-byte entries, used by dynamic props), and names the reason.
- It does not promise to be panic-free on arbitrary bytes, but it is built to be. Library code denies unchecked indexing and slicing, unchecked integer arithmetic and narrowing casts (`clippy::indexing_slicing`, `arithmetic_side_effects` and `cast_possible_truncation` in `src/lib.rs`). A stored offset, count or size is read through a checked read, and a value that does not fit is an `Error`. A few items allow one of these lints, each with its reason in the source: the tangent codec (indexing into three-element arrays, quantising casts), two casts in the EBX encoder (taken after the value is range-checked), and the narrowing of computed floats (and one clamped bin index) in `physics` and `rigid`. The overflows found before the lints were added, and the allocation and nesting bounds, are pinned in `tests/robust.rs`.
- Whole files are read into memory; there is no streaming reader.
