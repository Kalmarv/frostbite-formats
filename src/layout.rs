//! `layout.toc`: the native DB (behind the TOC envelope) that names the superbundles, the install chunks that give the
//! archives' directories their `persistentIndex`, the initfs and the bundle format. [`read`] types the keys the stock
//! layout and the 98 layouts of installed mods hold; the tree stays reachable as [`Layout::root`] for the rest
//! (`layeredInstallChunkFiles`, `meta`, ...).
//!
//! Every key below occurs in the stock layout (a measured build 20260929) and in all 98 mod layouts read beside it; no
//! mod layout holds a key the stock one does not. A key of another type than the one named here is refused.

use crate::db::{self, Node};
use crate::{Result, bad};

const W: &str = "layout.toc";

/// An install chunk (`installManifest.installChunks[]`): the directory of CAS archives that `persistentIndex` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallChunk {
    /// `id`: a GUID, in the byte order stored.
    pub id: Option<[u8; 16]>,
    /// `name`: the directory relative to `Win32`, as stored (`configurations/layout/initialinstallpackage`).
    pub name: String,
    /// `installBundle`: the same directory with the platform in front (`Win32/` + `name` in all 16 stock chunks).
    pub install_bundle: Option<String>,
    /// `persistentIndex`: the number a file identifier's bits 16..48 name this chunk by.
    pub persistent_index: Option<u32>,
    /// `superbundles`: the superbundle names (`Win32/loc/br`) the chunk holds.
    pub superbundles: Vec<String>,
    /// `requiredChunks`: the `id`s of the chunks this one needs.
    pub required_chunks: Vec<[u8; 16]>,
    /// `alwaysInstalled`.
    pub always_installed: Option<bool>,
}

impl InstallChunk {
    /// The directory of the chunk's archives relative to `Win32`: `name`, else `installBundle` without its `Win32/`.
    pub fn directory(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        self.install_bundle
            .as_deref()
            .map(|b| b.strip_prefix("Win32/").unwrap_or(b).to_string())
            .unwrap_or_default()
    }
}

/// An install group (`installManifest.installGroups[]`): a name and the `id`s of its chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallGroup {
    /// `name`.
    pub name: String,
    /// `chunks`: install chunk ids.
    pub chunks: Vec<[u8; 16]>,
}

/// A read `layout.toc`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The envelope bytes in front of the DB, as stored.
    pub envelope: Vec<u8>,
    /// The whole DB tree.
    pub root: Node,
    /// The bytes after the tree (none in the measured layouts).
    pub trailer: Vec<u8>,
    /// `superBundles[].name`: the superbundles the game loads, as `Win32/globals`; the TOC of each is `<name>.toc` in the
    /// data folder. In the stock layout, 23 of 24 names have their TOC in `Data` and every TOC there is named.
    pub super_bundles: Vec<String>,
    /// `fs`: the initfs files (`initfs_Win32`), which are encrypted.
    pub fs: Vec<String>,
    /// `head`: a 32-bit number (625,200 in the stock layout and all mod layouts).
    pub head: Option<u32>,
    /// `platform`.
    pub platform: Option<String>,
    /// `installManifest.installChunks`, in stored order.
    pub install_chunks: Vec<InstallChunk>,
    /// `installManifest.installGroups`.
    pub install_groups: Vec<InstallGroup>,
    /// `installManifest.excludedSuperbundles`.
    pub excluded_superbundles: Vec<String>,
    /// `installManifest.settings.bundleFormat`: a 64-bit number as stored.
    pub bundle_format: Option<u64>,
}

fn err(detail: impl Into<String>) -> crate::Error {
    bad(W, 0, detail)
}

fn kind(n: &Node, ty: u8, key: &str) -> Result<()> {
    if n.ty == ty {
        Ok(())
    } else {
        Err(err(format!("{key}: expected type {ty}, found {}", n.ty)))
    }
}

fn text(parent: &Node, key: &str) -> Result<Option<String>> {
    parent
        .field(key)
        .map(|n| kind(n, 7, key).map(|()| n.text()))
        .transpose()
}

fn guid(n: &Node, key: &str) -> Result<[u8; 16]> {
    kind(n, 15, key)?;
    n.payload
        .as_slice()
        .try_into()
        .map_err(|_| err(format!("{key}: expected 16 bytes")))
}

fn strings(parent: &Node, key: &str) -> Result<Vec<String>> {
    let Some(list) = parent.field(key) else {
        return Ok(Vec::new());
    };
    kind(list, 1, key)?;
    list.children
        .iter()
        .map(|c| kind(c, 7, key).map(|()| c.text()))
        .collect()
}

fn guids(parent: &Node, key: &str) -> Result<Vec<[u8; 16]>> {
    let Some(list) = parent.field(key) else {
        return Ok(Vec::new());
    };
    kind(list, 1, key)?;
    list.children.iter().map(|c| guid(c, key)).collect()
}

/// Parse a `layout.toc` (envelope included). Refused: an envelope or DB that does not read, and a key of the typed set
/// that has another type than the stock layout gives it.
pub fn read(data: &[u8]) -> Result<Layout> {
    let (envelope, root, trailer) = db::read_file(data)?;
    let super_bundles = match root.field("superBundles") {
        None => Vec::new(),
        Some(list) => {
            kind(list, 1, "superBundles")?;
            list.children
                .iter()
                .map(|e| text(e, "name")?.ok_or_else(|| err("superBundles: an entry without a name")))
                .collect::<Result<_>>()?
        }
    };
    let head = match root.field("head") {
        None => None,
        Some(n) => Some(n.u32().ok_or_else(|| err("head: expected a 32-bit value"))?),
    };
    let manifest = root.field("installManifest");
    let mut install_chunks = Vec::new();
    let mut install_groups = Vec::new();
    let mut excluded_superbundles = Vec::new();
    let mut bundle_format = None;
    if let Some(m) = manifest {
        if let Some(list) = m.field("installChunks") {
            kind(list, 1, "installChunks")?;
            for e in &list.children {
                install_chunks.push(InstallChunk {
                    id: e.field("id").map(|n| guid(n, "id")).transpose()?,
                    name: text(e, "name")?.unwrap_or_default(),
                    install_bundle: text(e, "installBundle")?,
                    persistent_index: e
                        .field("persistentIndex")
                        .map(|n| {
                            n.u32()
                                .ok_or_else(|| err("persistentIndex: expected a 32-bit value"))
                        })
                        .transpose()?,
                    superbundles: strings(e, "superbundles")?,
                    required_chunks: guids(e, "requiredChunks")?,
                    always_installed: e
                        .field("alwaysInstalled")
                        .map(|n| match (n.ty, n.payload.as_slice()) {
                            (6, &[b]) => Ok(b != 0),
                            _ => Err(err("alwaysInstalled: expected a boolean")),
                        })
                        .transpose()?,
                });
            }
        }
        if let Some(list) = m.field("installGroups") {
            kind(list, 1, "installGroups")?;
            for e in &list.children {
                install_groups.push(InstallGroup {
                    name: text(e, "name")?.unwrap_or_default(),
                    chunks: guids(e, "chunks")?,
                });
            }
        }
        excluded_superbundles = strings(m, "excludedSuperbundles")?;
        if let Some(b) = m.field("settings").and_then(|s| s.field("bundleFormat")) {
            bundle_format = Some(match (b.ty, b.payload.as_slice()) {
                (9, &[a, b, c, d, e, f, g, h]) => u64::from_le_bytes([a, b, c, d, e, f, g, h]),
                _ => return Err(err("bundleFormat: expected a 64-bit value")),
            });
        }
    }
    Ok(Layout {
        envelope,
        super_bundles,
        fs: strings(&root, "fs")?,
        head,
        platform: text(&root, "platform")?,
        install_chunks,
        install_groups,
        excluded_superbundles,
        bundle_format,
        root,
        trailer,
    })
}

impl Layout {
    /// The layout written again: the envelope, the tree and the trailer as read ([`db::write_file`]).
    pub fn write(&self) -> Result<Vec<u8>> {
        db::write_file(&self.envelope, &self.root, &self.trailer)
    }
}
