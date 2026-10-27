# Changelog

## 0.1.0 (unreleased)

- Fixed: a debug-build overflow panic on a stored value in `toc::lookup` (hash-map entry `i32::MIN`), `physics::read` (face offset `0xFFFF`) and `binding::Group::buffer_end`, which now returns a `u64` so a stored offset near `u32::MAX` is reported whole.
- Library code denies `clippy::indexing_slicing`, `arithmetic_side_effects` and `cast_possible_truncation`; readers go through checked reads (`Rd::size32le`, `Rd::offset64le`, `at`, `get()`), and a count or size that does not fit its field is an error. Changed public items: `Bundle::ebx` and `Bundle::resource` return `Option<Listed>`; `bundle::write_region`, `depot::write` and `lookup::write_programs` return `Result`; `ebx::carry::compact` and `order_exported_first` return `Result`; `Selector::walk` can return the new `WalkError::Malformed`.
- `depot::read` refuses blocks that together hold more bytes than the payload; `Cas::decompress` refuses raw blocks that claim more than they hold before reserving the stream.
- `skeleton::verify_hierarchy` returns the crate's `Error` instead of a `String`; `toc::read` reports offsets into the whole file, envelope included, and says what it expected.
- First release: readers and writers for the TOC, bundle, CAS, native DB, EBX, `.fbmod`, shader, MeshSet, physics, skeleton and texture formats of skate.
- `ebx::remap` gives a document's identities (file, instance, import and GUID values, and the bytes it carries) new GUIDs, so a clone and its original can both be loaded; `guid_text` writes a GUID in its text form.
