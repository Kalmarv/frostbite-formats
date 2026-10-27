//! CompiledBytecode resources (type [`RTYPE`], named `shaders/bytecode/<guid>`): a program's compiled form as skate.'s
//! shader database holds it. A resource is a relocatable payload (its 16-byte metadata gives the payload size, then
//! the relocation size) that begins with a header and holds one DXBC container with a DXIL part (Shader Model 6), or
//! two (a raytracing pipeline's, whose header names `RPTR` at offset 0x20 where the others name `RC2S`); more bytes
//! follow a container in most resources. The header's fields are not interpreted here: the bytes before the first
//! container and after each container are kept as stored. A container is found by its magic and delimited by its own
//! size field; a magic that is not a container is an error, not skipped.

use crate::{Error, Rd, Result, bad};

const W: &str = "compiled bytecode";
/// The resource type id of a CompiledBytecode resource.
pub const RTYPE: u32 = 0x0E2B_02F7;
/// The offset of the four-byte tag (`RC2S`, `RPTR`) a payload's header carries.
pub const TAG_AT: usize = 0x20;
const MAGIC: &[u8; 4] = b"DXBC";

/// A container's place in a payload: its offset and its size, as its own header states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// Byte offset of the container's `DXBC` magic within the payload.
    pub offset: usize,
    /// The container's size in bytes, from its own header (offset 24); the container ends at `offset + size`.
    pub size: usize,
}

/// The payload's tag at [`TAG_AT`], if it has one.
pub fn tag(payload: &[u8]) -> Option<[u8; 4]> {
    payload.get(TAG_AT..)?.first_chunk().copied()
}

/// A container header at `at`: the magic, version 1.0, a size that holds the part directory and lies within the
/// payload, and a directory of parts that lie within the container. `Ok(size)` when it is one.
fn header(payload: &[u8], at: usize) -> Result<usize> {
    let r = Rd::new(payload, W);
    let word = |o: usize| -> Result<usize> {
        let w = r.u32le(at.saturating_add(o)).map_err(|_| {
            bad(
                W,
                at.saturating_add(o),
                "a container header past the end: expected a 32-bit word",
            )
        })?;
        usize::try_from(w).map_err(|_| bad(W, at.saturating_add(o), "word does not fit the address space"))
    };
    if word(20)? != 1 {
        return Err(bad(W, at, format!("version {:#x}, not 1", word(20)?)));
    }
    let (size, n) = (word(24)?, word(28)?);
    let directory = n.checked_mul(4).and_then(|d| d.checked_add(32));
    if directory.is_none_or(|d| d > size) || at.checked_add(size).is_none_or(|e| e > payload.len()) {
        return Err(bad(
            W,
            at,
            format!("size {size} with {n} parts in a payload of {}", payload.len()),
        ));
    }
    let directory_end = directory.unwrap_or(usize::MAX);
    for i in 0..n {
        let part = word(crate::at(32, i, 4))?;
        if part < directory_end || part.saturating_add(8) > size {
            return Err(bad(W, at, format!("part {i} at {part} in a container of {size}")));
        }
    }
    Ok(size)
}

/// Every container of `payload`, in order. A container's bytes are not searched for another; a magic outside a
/// container that does not head one is an error naming its offset, and so is a payload with none.
pub fn containers(payload: &[u8]) -> Result<Vec<Span>> {
    let (mut out, mut from) = (Vec::new(), 0);
    while let Some(p) = payload
        .get(from..)
        .and_then(|rest| rest.windows(4).position(|w| w == MAGIC))
    {
        let offset = from.saturating_add(p);
        let size = header(payload, offset)?;
        out.push(Span { offset, size });
        from = offset.saturating_add(size);
    }
    if out.is_empty() {
        return Err(Error::Unsupported(format!(
            "{W}: no DXBC container in {} bytes",
            payload.len()
        )));
    }
    Ok(out)
}
