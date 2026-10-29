//! Shader parameter keys: the 16-byte identity the game gives every shader parameter, and the names behind them.
//!
//! A [`ParamKey`] is stored in two places that agree byte for byte: as the first 16 bytes of every parameter record of a
//! shader block depot ([`depot::Param`](crate::depot::Param)) and as the `Data` GUID of the `ParamDbKeySerialized`
//! structure that material presets, Lua shader bindings, texture-compositor parameters and others carry in their EBX
//! documents. Its fields:
//!
//! | bytes | field | meaning |
//! |---|---|---|
//! | 0..8 | [`ParamKey::hash`] | the low 16 bits of the key's name hash in its top 16 bits, 48 more bits of a second hash below them |
//! | 8..12 | [`ParamKey::type_key`] | the reflection name key of the value's type (`0x14c5fc30` for a texture reference) |
//! | 12..14 | [`ParamKey::count`] | the element count |
//! | 14..16 | [`ParamKey::identity`] | the high 16 bits of the key's name hash |
//!
//! # The name hash
//!
//! The 32-bit [name hash](name_hash) of a parameter is the game's lower-cased djb2-XOR hash (multiplier 33, each byte
//! XORed in after the multiplication) of the name of the parameter's `ShaderExpressionParameter` asset, the
//! document name as the bundle manifests state it (`common/shadercomponents/parameters/base/param_base_c`), started
//! from [`NAME_HASH_SEED`]. Its low half is the top 16 bits of [`ParamKey::hash`] and its high half is
//! [`ParamKey::identity`]. This is what the census of the stock install measured: all 207 distinct keys that the stock
//! documents tie to a parameter asset (40,262 uses of them, each key to one asset) satisfy it, and 195 of those 207 are
//! depot parameter prefixes, equal in type and identity.
//!
//! What the other 48 bits of [`ParamKey::hash`] are is **not known**: they are no CityHash64, FarmHash, xxHash,
//! Murmur or FNV hash of the name in the layouts tried, so [`ParamKey::from_parts`] takes them from the caller. A
//! writer that needs a key for a new parameter name can compose everything but those bits; for a parameter the game
//! already has, the stock key is the only source of them.

use std::collections::BTreeMap;

use crate::ebx::{Document, Object, Pointer, Value};
use crate::names::GuidNames;
use crate::{Rd, Result};

const W: &str = "shader parameter key";

/// The seed of [`name_hash`]. Only its value is known (it reproduces the name hash of every key of the stock install that
/// names a parameter asset); how the game arrives at it is not.
pub const NAME_HASH_SEED: u32 = 0x6875_ca4d;

/// The mask of the 48 bits of [`ParamKey::hash`] that are not the name hash.
pub const LOW48: u64 = 0x0000_ffff_ffff_ffff;

/// The 32-bit name hash of a parameter: djb2-XOR over the ASCII-lower-cased bytes of `name`, started from
/// [`NAME_HASH_SEED`].
pub fn name_hash(name: &[u8]) -> u32 {
    name.iter().fold(NAME_HASH_SEED, |h, &c| {
        h.wrapping_mul(33) ^ u32::from(c.to_ascii_lowercase())
    })
}

/// A shader parameter key (see the [module](self)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParamKey {
    /// Bytes 0..8: the low half of the [name hash](name_hash) in the top 16 bits, the other 48 bits unexplained.
    pub hash: u64,
    /// Bytes 8..12: the reflection name key of the value's type.
    pub type_key: u32,
    /// Bytes 12..14: the element count (1 for a single value).
    pub count: u16,
    /// Bytes 14..16: the high half of the [name hash](name_hash).
    pub identity: u16,
}

impl ParamKey {
    /// A key from its 16 stored bytes.
    pub fn from_bytes(b: &[u8; 16]) -> Result<ParamKey> {
        let r = Rd::new(b, W);
        Ok(ParamKey {
            hash: r.u64le(0)?,
            type_key: r.u32le(8)?,
            count: r.u16le(12)?,
            identity: r.u16le(14)?,
        })
    }

    /// The 16 bytes the key is stored as.
    pub fn to_bytes(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        let (head, tail) = out.split_at_mut(8);
        head.copy_from_slice(&self.hash.to_le_bytes());
        let (ty, rest) = tail.split_at_mut(4);
        ty.copy_from_slice(&self.type_key.to_le_bytes());
        let (count, identity) = rest.split_at_mut(2);
        count.copy_from_slice(&self.count.to_le_bytes());
        identity.copy_from_slice(&self.identity.to_le_bytes());
        out
    }

    /// A key from the [name hash](name_hash) of its parameter and the 48 bits of [`ParamKey::hash`] below it.
    pub fn from_parts(name_hash: u32, low48: u64, type_key: u32, count: u16) -> ParamKey {
        let low = u64::from(name_hash & 0xffff);
        ParamKey {
            hash: (low << 48) | (low48 & LOW48),
            type_key,
            count,
            identity: u16::try_from(name_hash >> 16).unwrap_or(u16::MAX),
        }
    }

    /// The name hash the key carries: [`ParamKey::identity`] above the top 16 bits of [`ParamKey::hash`].
    pub fn name_hash(&self) -> u32 {
        let low = u16::try_from(self.hash >> 48).unwrap_or(u16::MAX);
        (u32::from(self.identity) << 16) | u32::from(low)
    }

    /// The 48 bits of [`ParamKey::hash`] below the name hash's half.
    pub fn low48(&self) -> u64 {
        self.hash & LOW48
    }

    /// Whether the key's name hash is the [`name_hash`] of `name`. Only a name that is the parameter asset's own name
    /// can satisfy it; a Lua alias or a display name does not.
    pub fn is_named(&self, name: &[u8]) -> bool {
        self.name_hash() == name_hash(name)
    }
}

/// Where the name beside a key in an EBX document comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyUse {
    /// The key.
    pub key: ParamKey,
    /// The file GUID of the `ShaderExpressionParameter` asset the key's owner points at (its `Parameter` field, directly
    /// or in a structure beside the key), if it points at one; [`GuidNames`] gives the asset's name.
    pub asset: Option<[u8; 16]>,
    /// The string in a `Name`, `ParamName` or `ParameterName` field of the key's owner, if any: a name the document
    /// uses for the key (a Lua binding's alias), which is not the asset's name and need not hash to the key.
    pub alias: Option<String>,
}

const KEY_TYPE: &str = "ParamDbKeySerialized";
const ALIAS_FIELDS: [&str; 3] = ["Name", "ParamName", "ParameterName"];

/// Every non-zero `ParamDbKeySerialized` in the instances of `doc`, with what stands beside it.
pub fn key_uses(doc: &Document) -> Vec<KeyUse> {
    let mut out = Vec::new();
    for o in doc.instances.iter().filter_map(|i| i.object.as_ref()) {
        visit(doc, o, &mut out);
    }
    out
}

fn visit(doc: &Document, o: &Object, out: &mut Vec<KeyUse>) {
    let keys: Vec<ParamKey> = o
        .fields
        .iter()
        .filter_map(|f| key_of(doc, &f.value))
        .filter(|k| *k != ParamKey::default())
        .collect();
    if !keys.is_empty() {
        let asset = parameter_asset(doc, o);
        let alias = ALIAS_FIELDS.iter().find_map(|name| match o.field(name) {
            Some(Value::Str(b)) if !b.is_empty() => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        });
        for key in keys {
            out.push(KeyUse {
                key,
                asset,
                alias: alias.clone(),
            });
        }
    }
    for f in &o.fields {
        descend(doc, &f.value, out);
    }
}

fn descend(doc: &Document, v: &Value, out: &mut Vec<KeyUse>) {
    match v {
        Value::Object(o) => visit(doc, o, out),
        Value::Array(items, _) => {
            for item in items {
                descend(doc, item, out);
            }
        }
        _ => {}
    }
}

fn key_of(doc: &Document, v: &Value) -> Option<ParamKey> {
    let Value::Object(o) = v else { return None };
    if doc.types.get(o.descriptor)?.name != KEY_TYPE {
        return None;
    }
    let Value::Guid(g) = o.field("Data")? else {
        return None;
    };
    ParamKey::from_bytes(g).ok()
}

/// The file GUID of the document an external pointer in field `Parameter` of `o` (or of a structure in one of its
/// fields) names.
fn parameter_asset(doc: &Document, o: &Object) -> Option<[u8; 16]> {
    let external = |v: Option<&Value>| match v {
        Some(Value::Pointer(Pointer::External(i))) => doc.imports.get(*i).map(|imp| imp.file_guid),
        _ => None,
    };
    external(o.field("Parameter")).or_else(|| {
        o.fields.iter().find_map(|f| match &f.value {
            Value::Object(inner) => external(inner.field("Parameter")),
            _ => None,
        })
    })
}

/// What [`ParamNames::add`] did with a batch of key uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Added {
    /// Keys given a name they did not have.
    pub added: usize,
    /// Key uses whose asset is named by the table and whose key already had that name.
    pub repeated: usize,
    /// Key uses that point at an asset the table of file GUIDs does not name.
    pub unnamed: usize,
    /// Key uses whose asset name does not hash to the key's name hash, or names a key that already has another name:
    /// refused, the first name stays.
    pub refused: usize,
}

/// Parameter key -> the name of the parameter asset it belongs to, kept only when the name hashes to the key's name hash.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParamNames {
    names: BTreeMap<ParamKey, String>,
}

impl ParamNames {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the asset names of `uses` from `files`. A name is kept when it hashes to its key's name hash
    /// ([`ParamKey::is_named`]) and the key has no other name.
    pub fn add(&mut self, uses: &[KeyUse], files: &GuidNames) -> Added {
        let mut added = Added::default();
        for u in uses {
            let Some(guid) = u.asset else { continue };
            let Some(name) = files.get(&guid) else {
                added.unnamed = added.unnamed.saturating_add(1);
                continue;
            };
            if !u.key.is_named(name.as_bytes()) {
                added.refused = added.refused.saturating_add(1);
                continue;
            }
            match self.names.get(&u.key) {
                None => {
                    self.names.insert(u.key, name.to_string());
                    added.added = added.added.saturating_add(1);
                }
                Some(old) if old == name => added.repeated = added.repeated.saturating_add(1),
                Some(_) => added.refused = added.refused.saturating_add(1),
            }
        }
        added
    }

    /// The name of the parameter whose key is `key`.
    pub fn get(&self, key: &ParamKey) -> Option<&str> {
        self.names.get(key).map(String::as_str)
    }

    /// The name of the parameter whose key has hash `hash` (the first by key order when the table holds two keys of one hash).
    pub fn by_hash(&self, hash: u64) -> Option<&str> {
        self.names
            .iter()
            .find(|(k, _)| k.hash == hash)
            .map(|(_, n)| n.as_str())
    }

    /// The number of keys named.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether no key is named.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Every (key, name), in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&ParamKey, &str)> + '_ {
        self.names.iter().map(|(k, n)| (k, n.as_str()))
    }
}
