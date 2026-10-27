//! New GUIDs for a document's identities ([`remap`]): what a clone of a set of documents takes, so the clone and its
//! original can both be loaded. The documents' own identities ([`defined`], and any other GUID their values define,
//! such as an ECS prefab's entity GUIDs) are given new ones by a map, and every place a GUID of the map is stored follows
//! it: the file GUID, instance GUIDs, import references, GUID values, and the bytes the document carries as read
//! (instance images, boxed values, the payload), so a GUID inside a value the reader does not decode follows too.
//! GUIDs outside the map (type GUIDs, references to other documents) stay.
use std::collections::HashMap;

use super::carry::{flags_word, object_id};
use super::{Document, Value};

/// The object id bits of an exported instance's `Flags` (the low bits of its GUID, as [`super::carry`] keeps them).
const OBJECT_ID_MASK: u32 = 0x01FF_FFFF;

/// The GUIDs `doc` defines: its file GUID, then its instances' GUIDs.
pub fn defined(doc: &Document) -> Vec<[u8; 16]> {
    std::iter::once(doc.file_guid)
        .chain(doc.instances.iter().map(|i| i.guid))
        .collect()
}

/// Every GUID value inside `v`, mutably.
fn walk_guids(v: &mut Value, f: &mut dyn FnMut(&mut [u8; 16])) {
    match v {
        Value::Guid(g) => f(g),
        Value::Object(o) => o.fields.iter_mut().for_each(|x| walk_guids(&mut x.value, f)),
        Value::Array(items, _) => items.iter_mut().for_each(|x| walk_guids(x, f)),
        _ => {}
    }
}

/// Every GUID value of `doc`'s objects, in instance and field order (repeats kept).
pub fn values(doc: &mut Document) -> Vec<[u8; 16]> {
    let mut out = Vec::new();
    for inst in &mut doc.instances {
        if let Some(o) = &mut inst.object {
            o.fields
                .iter_mut()
                .for_each(|x| walk_guids(&mut x.value, &mut |g| out.push(*g)));
        }
    }
    out
}

/// Replaces every 16-byte run of `bytes` that is a key of `map` with its value; how many.
fn replace_bytes(bytes: &mut [u8], map: &HashMap<[u8; 16], [u8; 16]>) -> usize {
    let mut n = 0usize;
    let mut at = 0usize;
    while let Some(window) = bytes.get_mut(at..at.saturating_add(16)) {
        let Ok(w) = <[u8; 16]>::try_from(&*window) else {
            break;
        };
        match map.get(&w) {
            Some(new) => {
                window.copy_from_slice(new);
                n = n.saturating_add(1);
                at = at.saturating_add(16);
            }
            None => at = at.saturating_add(1),
        }
    }
    n
}

/// Gives every GUID of `doc` that is a key of `map` its value (the module documentation's places). An exported
/// instance whose `Flags` held its old GUID's object id gets the new one's. How many places changed.
pub fn remap(doc: &mut Document, map: &HashMap<[u8; 16], [u8; 16]>) -> usize {
    let mut n = 0usize;
    let mut swap = |g: &mut [u8; 16]| {
        if let Some(new) = map.get(g) {
            *g = *new;
            n = n.saturating_add(1);
        }
    };
    swap(&mut doc.file_guid);
    for imp in &mut doc.imports {
        swap(&mut imp.file_guid);
        swap(&mut imp.class_guid);
    }
    for inst in &mut doc.instances {
        let old = inst.guid;
        swap(&mut inst.guid);
        if let Some(o) = &mut inst.object {
            o.fields
                .iter_mut()
                .for_each(|x| walk_guids(&mut x.value, &mut swap));
            if inst.exported && inst.guid != old {
                renumber(o, &old, &inst.guid);
            }
        }
    }
    for inst in &mut doc.instances {
        n = n.saturating_add(replace_bytes(&mut inst.raw_image, map));
    }
    for b in &mut doc.boxed_values {
        n = n.saturating_add(replace_bytes(&mut b.raw, map));
    }
    n.saturating_add(replace_bytes(&mut doc.payload, map))
}

/// The object id in `Flags` follows the instance's GUID, when it was the old GUID's.
fn renumber(obj: &mut super::Object, old: &[u8; 16], new: &[u8; 16]) {
    let Some(f) = obj.fields.iter_mut().find(|f| f.name == "Flags") else {
        return;
    };
    let Some(current) = flags_word(&f.value) else {
        return;
    };
    if current & OBJECT_ID_MASK != object_id(old) & OBJECT_ID_MASK {
        return;
    }
    let next = (current & !OBJECT_ID_MASK) | (object_id(new) & OBJECT_ID_MASK);
    f.value = match f.value {
        Value::Int(_) => Value::Int(i64::from(next.cast_signed())),
        _ => Value::UInt(u64::from(next)),
    };
}
