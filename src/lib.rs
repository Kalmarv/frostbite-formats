//! Readers and writers for the container and resource formats of EA Frostbite as *skate.* (2025) stores them: bytes
//! in, plain records out, and the same records back to bytes.
//!
//! # What it reads
//!
//! - **Archives and bundles:** superbundle TOCs ([`toc`]), a bundle's file table and manifest ([`bundle`]), CAS
//!   archives, block by block and decoded ([`cas`]), the native DB behind `layout.toc` ([`db`]) and `layout.toc`
//!   itself typed ([`layout`]), a data folder and the bundles it holds ([`folder`], [`listing`], [`scope`]), a table
//!   from EBX file GUIDs to document names ([`names`]), and ReSkate Studio's `.fbmod` mods ([`fbmod`]).
//! - **EBX documents:** read into typed values, written again, and carried between documents ([`ebx`]).
//! - **Shaders:** block depots ([`depot`]), the program and texture lookup tables ([`lookup`]), the permutation
//!   selector ([`selector`]), permutations and their fixed-function state ([`permutation`]), binding sets
//!   ([`binding`]), input layouts ([`inputlayout`]) and compiled bytecode containers ([`bytecode`]).
//! - **Geometry and animation:** MeshSets ([`meshset`]), rigid meshes built from triangles ([`rigid`]), the packed
//!   tangent frame ([`tangent`]), physics resources ([`physics`]), skeleton assets ([`skeleton`]) and texture
//!   headers ([`texture`]).
//!
//! It was written against the skate. build installed from the game and the files ReSkate Studio (20261012) writes;
//! other Frostbite titles are not assumed to match. It is an independent reader of the stored formats with no code
//! from Frosty or FrostyToolsuite, except that the [`ebx`] module is a port of ReSkate's EBX reader, writer and merge
//! (GPL-3.0-or-later).
//!
//! # What it guarantees
//!
//! - **Checked reads.** Readers are bounds-checked: a file that is truncated or does not hold to the format returns
//!   an [`Error`] that names what was being read, the offset and what was expected. Counts read from a file are checked
//!   against the bytes that remain before a table is reserved for them. Library code denies unchecked indexing,
//!   arithmetic and narrowing casts, so a stored offset, count or size that does not fit is an error, not a panic;
//!   the crate does not promise to be panic-free on arbitrary bytes, and the few items that allow one of those lints
//!   say why.
//! - **Strict readers.** They walk to the exact end of what they read and refuse a value they do not know instead of
//!   guessing; fields whose meaning is not known are kept as stored.
//! - **Writers are the readers' counterparts.** What a reader accepts is written back byte for byte (TOCs only in the
//!   Patch layout, with plain names; an EBX document when it already has the writer's layout), or the writer says
//!   why it cannot.
//! - **No game data, no hidden I/O.** The crate holds no game data and no keys, and touches files only in [`folder`].
//!   Oodle-compressed CAS blocks are decoded through the caller's own handle to the game's Oodle library
//!   ([`cas::Cas`]); the library is not shipped.
//!
//! It does not decode pixels, vertices or shader code (only the headers, tables and records around them), does not
//! read other Frostbite games, and reads whole files into memory.
//!
//! # Example
//!
//! Build a one-bundle superbundle TOC, read it back, and check that the bundle and chunk lookups resolve:
//!
//! ```
//! use frostbite_formats::bundle;
//! use frostbite_formats::toc::{self, Loc, TocChunk};
//!
//! # fn main() -> Result<(), frostbite_formats::Error> {
//! let archive = Loc { patch: true, chunk: 0, archive: 1 };
//! // a bundle whose region lists one file: 16 bytes at offset 0 of the archive
//! let region = bundle::write_region(&[(archive, 0, 16)], &[])?;
//! let chunk = TocChunk { guid: [7; 16], file: Some((archive, 16, 64)) };
//!
//! let bytes = toc::write(&[("win32/levels/demo".to_string(), region)], &[chunk], 0)?;
//!
//! let parsed = toc::read(&bytes)?;
//! toc::verify(&parsed)?;
//! assert_eq!(parsed.bundles[0].0, "win32/levels/demo");
//! let (files, _inline_manifest) = bundle::read_region(&parsed.bundles[0].1)?;
//! assert_eq!(files, [(archive, 0, 16)]);
//! assert_eq!(parsed.chunks[0].file, Some((archive, 16, 64)));
//!
//! // a corrupt file is refused with where and why
//! let err = toc::read(&bytes[..100]).unwrap_err();
//! assert!(err.to_string().contains("toc at 0x0"));
//! # Ok(())
//! # }
//! ```
#![deny(missing_docs, clippy::disallowed_methods, clippy::disallowed_types)]
// Stored bytes never panic a reader: no unchecked index, slice, arithmetic or narrowing cast in library code.
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation
    )
)]

use primase_foundations::bytes::Endian;

pub mod binding;
pub mod bundle;
pub mod bytecode;
pub mod cas;
pub mod db;
pub mod depot;
pub mod ebx;
pub mod fbmod;
pub mod folder;
pub mod inputlayout;
pub mod layout;
pub mod listing;
pub mod lookup;
pub mod meshset;
pub mod names;
pub mod permutation;
pub mod physics;
pub mod rigid;
pub mod scope;
pub mod selector;
pub mod skeleton;
pub mod tangent;
pub mod texture;
pub mod toc;

/// Resource type of a MeshSet.
pub const MESHSET: u32 = 0x49B1_56D4;
/// Resource type of a texture.
pub const TEXTURE: u32 = 0x6BDE_20BA;

/// What went wrong reading or writing a format: the format and offset that broke, a file that could not be opened, a
/// CAS block that would not decode, or an entry of a kind no reader here reads.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Bytes that do not hold to the format: what was being read, the offset (within the bytes being read) where it
    /// went wrong, and what was expected.
    #[error("{what} at {offset:#x}: {detail}")]
    Format {
        /// The thing being read (`"toc"`, `"meshset"`), the first word of the message.
        what: &'static str,
        /// Byte offset of the failure within the bytes that were being read, not within the file they came from.
        offset: usize,
        /// What was expected or what was found instead.
        detail: String,
    },
    /// A file or folder that could not be read.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or folder that failed.
        path: std::path::PathBuf,
        /// The operating system's error.
        source: std::io::Error,
    },
    /// A CAS block that would not decode (or an Oodle library that was needed and not given).
    #[error("cas: {0}")]
    Cas(#[from] primase_foundations::compress::Error),
    /// Something a lookup needed and the data does not hold (a bundle, a chunk, an archive), named.
    #[error("{0}")]
    Missing(String),
    /// An entry of a kind or form no reader here reads, named.
    #[error("{0}")]
    Unsupported(String),
}

/// `Result` with this crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Frostbite's string hash: DJB2 with xor (`h = 5381`, then `h = h * 33 ^ byte` over the name's bytes, 32 bits), case as
/// given. The `NameHash` of assets (a MeshSet's header word 0x78, a RigidMeshAsset's `NameHash`), the name hashes of
/// MeshSet records, and (lowercased) resource type ids ([`listing::type_id`]).
pub fn djb2(name: &str) -> u32 {
    name.bytes()
        .fold(5381u32, |h, b| h.wrapping_mul(33) ^ u32::from(b))
}

/// A GUID's text form (`8-4-4-4-12`) from its stored bytes (the first three groups little-endian): how a mod names a
/// chunk entry by its GUID.
pub fn guid_text(g: &[u8; 16]) -> String {
    let [a0, a1, a2, a3, b0, b1, c0, c1, d0, d1, e0, e1, e2, e3, e4, e5] = *g;
    format!(
        "{a3:02x}{a2:02x}{a1:02x}{a0:02x}-{b1:02x}{b0:02x}-{c1:02x}{c0:02x}-{d0:02x}{d1:02x}-{e0:02x}{e1:02x}{e2:02x}{e3:02x}{e4:02x}{e5:02x}"
    )
}

/// Lowercase hexadecimal of `bytes`, in the order given.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(
        String::with_capacity(bytes.len().saturating_mul(2)),
        |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        },
    )
}

/// A format error: what was being read, where, and what broke.
pub fn bad(what: &'static str, offset: usize, detail: impl Into<String>) -> Error {
    Error::Format {
        what,
        offset,
        detail: detail.into(),
    }
}

/// A read past the data: `what`'s error at the read's offset, naming how many bytes it wanted and
/// the `total` there are (a span whose end overflows the address space wants all it names).
pub(crate) fn past_end(
    what: &'static str,
    e: primase_foundations::bytes::BoundsError,
    total: usize,
) -> Error {
    let at = usize::try_from(e.absolute()).unwrap_or(usize::MAX);
    let length = match e {
        primase_foundations::bytes::BoundsError::OutOfBounds { length, .. } => length,
        primase_foundations::bytes::BoundsError::LengthOverflow { count, stride, .. } => {
            count.saturating_mul(stride)
        }
    };
    bad(what, at, format!("{length} bytes past the end ({total} bytes)"))
}

/// A read past a document's bytes through [`primase_foundations::bytes::Cursor`] (the EBX decoder's): its error at the read's
/// offset.
impl From<primase_foundations::bytes::BoundsError> for Error {
    fn from(e: primase_foundations::bytes::BoundsError) -> Self {
        match e {
            primase_foundations::bytes::BoundsError::OutOfBounds { total, .. } => past_end("read", e, total),
            primase_foundations::bytes::BoundsError::LengthOverflow { count, stride, .. } => bad(
                "read",
                usize::try_from(e.absolute()).unwrap_or(usize::MAX),
                format!("{count} entries of {stride} bytes overflow"),
            ),
        }
    }
}

/// Bounds-checked fixed-size reads, little and big endian: every read past the end is an [`Error::Format`] that names
/// `what` and the offset.
#[derive(Debug)]
pub struct Rd<'a> {
    /// The bytes being read.
    pub data: &'a [u8],
    /// What they are, for error messages.
    pub what: &'static str,
}

impl<'a> Rd<'a> {
    /// Reads over `data`; `what` names the thing being read in every error.
    pub fn new(data: &'a [u8], what: &'static str) -> Self {
        Self { data, what }
    }

    /// `n` bytes at `at`.
    pub fn bytes(&self, at: usize, n: usize) -> Result<&'a [u8]> {
        primase_foundations::bytes::View::new(self.data)
            .slice(at, n)
            .map_err(|e| past_end(self.what, e, self.data.len()))
    }

    /// `N` bytes at `at` as an array.
    pub fn arr<const N: usize>(&self, at: usize) -> Result<[u8; N]> {
        primase_foundations::bytes::View::new(self.data)
            .array(at)
            .map_err(|e| past_end(self.what, e, self.data.len()))
    }

    /// The byte at `at`.
    pub fn u8(&self, at: usize) -> Result<u8> {
        primase_foundations::bytes::View::new(self.data)
            .u8(at)
            .map_err(|e| past_end(self.what, e, self.data.len()))
    }
    /// The little-endian `u16` at `at`.
    pub fn u16le(&self, at: usize) -> Result<u16> {
        self.number(at, Endian::Little, primase_foundations::bytes::View::u16)
    }
    /// The little-endian `u32` at `at`.
    pub fn u32le(&self, at: usize) -> Result<u32> {
        self.number(at, Endian::Little, primase_foundations::bytes::View::u32)
    }
    /// The little-endian `u64` at `at`.
    pub fn u64le(&self, at: usize) -> Result<u64> {
        self.number(at, Endian::Little, primase_foundations::bytes::View::u64)
    }
    /// The little-endian `u32` at `at` as a size or count.
    pub fn size32le(&self, at: usize) -> Result<usize> {
        let v = self.u32le(at)?;
        usize::try_from(v).map_err(|_| bad(self.what, at, format!("{v:#x} does not fit the address space")))
    }
    /// The little-endian `u64` at `at` as an offset or size.
    pub fn offset64le(&self, at: usize) -> Result<usize> {
        let v = self.u64le(at)?;
        usize::try_from(v).map_err(|_| bad(self.what, at, format!("{v:#x} does not fit the address space")))
    }
    /// The big-endian `u32` at `at`.
    pub fn u32be(&self, at: usize) -> Result<u32> {
        self.number(at, Endian::Big, primase_foundations::bytes::View::u32)
    }
    /// The big-endian `i32` at `at`.
    pub fn i32be(&self, at: usize) -> Result<i32> {
        self.number(at, Endian::Big, primase_foundations::bytes::View::i32)
    }
    /// The big-endian `u64` at `at`.
    pub fn u64be(&self, at: usize) -> Result<u64> {
        self.number(at, Endian::Big, primase_foundations::bytes::View::u64)
    }
    /// A number at `at` in byte order `e`, read by `read`.
    fn number<T>(
        &self,
        at: usize,
        e: Endian,
        read: fn(
            &primase_foundations::bytes::View<'a>,
            usize,
            Endian,
        ) -> std::result::Result<T, primase_foundations::bytes::BoundsError>,
    ) -> Result<T> {
        read(&primase_foundations::bytes::View::new(self.data), at, e)
            .map_err(|err| past_end(self.what, err, self.data.len()))
    }
    /// A zero-terminated Latin-1 string starting at `at`.
    pub fn cstr(&self, at: usize) -> Result<String> {
        let rest = self
            .data
            .get(at..)
            .ok_or_else(|| bad(self.what, at, "string past the end"))?;
        let mut parts = rest.splitn(2, |&b| b == 0);
        match (parts.next(), parts.next()) {
            (Some(text), Some(_)) => Ok(text.iter().map(|&b| char::from(b)).collect()),
            _ => Err(bad(self.what, at, "unterminated string, expected a zero byte")),
        }
    }
}

/// Latin-1 bytes of a name (Frostbite names are stored one byte per character).
pub fn latin1(s: &str, what: &'static str) -> Result<Vec<u8>> {
    s.chars()
        .map(|c| u8::try_from(u32::from(c)).map_err(|_| bad(what, 0, format!("name {s:?} is not Latin-1"))))
        .collect()
}

/// Capacity for `count` entries that each take at least `min_bytes` of the `remaining` input bytes: refused before
/// anything is reserved when they cannot fit, so a corrupt count returns an error instead of exhausting memory.
pub fn capacity(what: &'static str, count: usize, min_bytes: usize, remaining: usize) -> Result<usize> {
    match primase_foundations::bytes::checked_span(0, count, min_bytes, remaining) {
        Ok(_) => Ok(count),
        _ => Err(bad(
            what,
            0,
            format!("{count} entries of at least {min_bytes} bytes in {remaining} bytes"),
        )),
    }
}

/// `n` as the `u32` a format stores a count or size in; an error naming `what` when it does not fit.
pub fn u32_len(what: &'static str, n: usize) -> Result<u32> {
    u32::try_from(n).map_err(|_| {
        bad(
            what,
            0,
            format!("{n} does not fit the 32-bit field it is stored in"),
        )
    })
}

/// The offset of record `index` of a table at `base` whose records take `stride` bytes. Saturating: an offset past the
/// address space is past any data, and the read that uses it is refused.
pub fn at(base: usize, index: usize, stride: usize) -> usize {
    base.saturating_add(index.saturating_mul(stride))
}

/// `v` rounded up to a multiple of `a`.
pub fn align(v: usize, a: usize) -> usize {
    v.div_ceil(a).saturating_mul(a)
}

#[cfg(test)]
mod bounds_tests {
    use super::*;
    /// The fields of a format error, or a panic for any other kind: the constructors must choose both the kind and
    /// the offset, not only the message text.
    fn format_error(error: Error) -> (&'static str, usize, String) {
        match error {
            Error::Format { what, offset, detail } => (what, offset, detail),
            other => panic!("expected a format error, got {other}"),
        }
    }
    #[test]
    fn wrapper_retains_bounds_and_error_context() {
        let rd = Rd::new(&[1, 2, 3, 4], "bounds fixture");
        assert!(rd.bytes(4, 0).unwrap().is_empty());
        assert_eq!(rd.u32be(0).unwrap(), 0x01020304);
        for (offset, length) in [(4, 1), (5, 0), (usize::MAX, 1)] {
            let (what, at, detail) = format_error(rd.bytes(offset, length).unwrap_err());
            assert_eq!((what, at), ("bounds fixture", offset));
            assert_eq!(detail, format!("{length} bytes past the end (4 bytes)"));
        }
        assert_eq!(capacity("table", 2, 2, 4).unwrap(), 2);
        assert_eq!(capacity("table", usize::MAX, 0, 0).unwrap(), usize::MAX);
        for (count, stride) in [(3, 2), (usize::MAX, 2)] {
            let (what, at, detail) = format_error(capacity("table", count, stride, 4).unwrap_err());
            assert_eq!((what, at), ("table", 0));
            assert_eq!(
                detail,
                format!("{count} entries of at least {stride} bytes in 4 bytes")
            );
        }
    }
}
