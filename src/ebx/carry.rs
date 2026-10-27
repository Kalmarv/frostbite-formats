//! Moving values between documents and tidying a document after an edit (ReSkate's `ebx_merge.cpp` does the same
//! when it merges two documents' edits of one asset).
use std::collections::{BTreeMap, BTreeSet};

use super::{Document, FieldType, FieldValue, Instance, Object, Pointer, TypeRef, Value};
use crate::{Result, bad};

const W: &str = "EBX carry";
/// The object id bits of an instance's `Flags` (the low bits of its GUID).
const OBJECT_ID_MASK: u32 = 0x01FF_FFFF;

/// The EFIX type slot of `doc`'s type `descriptor`, added when the document has none (ReSkate's `ensure_fixup_type`).
fn ensure_fixup_type(doc: &mut Document, descriptor: usize) -> Result<u16> {
    let t = doc
        .types
        .get(descriptor)
        .ok_or_else(|| bad(W, 0, "type descriptor is invalid"))?;
    let (g, s) = (t.guid, t.signature);
    ensure_fixup_slot(doc, g, s)
}

/// The EFIX type slot of the type with GUID `g` and signature `s`, added when the document's list lacks it.
fn ensure_fixup_slot(doc: &mut Document, g: [u8; 16], s: u32) -> Result<u16> {
    let n = doc.fixup_type_guids.len().min(doc.fixup_type_signatures.len());
    let known = (doc.fixup_type_guids.iter().zip(&doc.fixup_type_signatures))
        .position(|(guid, signature)| *guid == g && *signature == s);
    if let Some(i) = known {
        return u16::try_from(i).map_err(|_| bad(W, 0, "type slot overflow"));
    }
    doc.fixup_type_guids.insert(n, g);
    doc.fixup_type_signatures.push(s);
    u16::try_from(n).map_err(|_| bad(W, 0, "type slot overflow"))
}

/// Every pointer inside `v`, mutably.
fn walk_pointers(v: &mut Value, f: &mut dyn FnMut(&mut Pointer)) {
    match v {
        Value::Pointer(p) => f(p),
        Value::Object(o) => o.fields.iter_mut().for_each(|x| walk_pointers(&mut x.value, f)),
        Value::Array(items, _) => items.iter_mut().for_each(|x| walk_pointers(x, f)),
        _ => {}
    }
}

/// Rewrites the pointers of the instances `keep` lists (indices into the document's instances) through `f`, which
/// gives a pointer's new target and `None` for one that has none; the first pointer without a target is an error.
fn remap_pointers(
    doc: &mut Document,
    keep: &[usize],
    kind: &'static str,
    f: &dyn Fn(&Pointer) -> Option<Pointer>,
) -> Result<()> {
    let mut missing = None;
    for &k in keep {
        let Some(o) = doc.instances.get_mut(k).and_then(|i| i.object.as_mut()) else {
            continue;
        };
        for fv in &mut o.fields {
            walk_pointers(&mut fv.value, &mut |p| match (f(p), &*p) {
                (Some(new), _) => *p = new,
                (None, Pointer::Internal(i) | Pointer::External(i)) => {
                    missing.get_or_insert(*i);
                }
                (None, Pointer::Null) => {}
            });
        }
    }
    match missing {
        None => Ok(()),
        Some(i) => Err(bad(
            W,
            i,
            format!("a pointer to {kind} {i} that the document does not keep or does not have"),
        )),
    }
}

/// Drops the instances nothing reachable from the root (instance 0) points at, and the imports nothing kept uses;
/// remaps every pointer. -> (instances kept, imports kept). An error when a kept instance points at an instance or
/// import the document does not have.
pub fn compact(doc: &mut Document) -> Result<(usize, usize)> {
    let mut reach = BTreeSet::new();
    let mut stack = vec![0usize];
    while let Some(k) = stack.pop() {
        if k >= doc.instances.len() || !reach.insert(k) {
            continue;
        }
        if let Some(o) = doc.instances.get_mut(k).and_then(|i| i.object.as_mut()) {
            for fv in &mut o.fields {
                walk_pointers(&mut fv.value, &mut |p| {
                    if let Pointer::Internal(i) = p {
                        stack.push(*i);
                    }
                });
            }
        }
    }
    let keep: Vec<usize> = (0..doc.instances.len()).filter(|k| reach.contains(k)).collect();
    let mut used = BTreeSet::new();
    for &k in &keep {
        if let Some(o) = doc.instances.get_mut(k).and_then(|i| i.object.as_mut()) {
            for fv in &mut o.fields {
                walk_pointers(&mut fv.value, &mut |p| {
                    if let Pointer::External(i) = p {
                        used.insert(*i);
                    }
                });
            }
        }
    }
    let ikeep: Vec<usize> = (0..doc.imports.len()).filter(|i| used.contains(i)).collect();
    let mmap: BTreeMap<usize, usize> = ikeep.iter().enumerate().map(|(n, &o)| (o, n)).collect();
    remap_pointers(doc, &keep, "import", &|p| match p {
        Pointer::External(i) => mmap.get(i).map(|&n| Pointer::External(n)),
        other => Some(*other),
    })?;
    rearrange(doc, &keep)?;
    let imports = std::mem::take(&mut doc.imports);
    doc.imports = ikeep.iter().filter_map(|&i| imports.get(i).copied()).collect();
    Ok((keep.len(), ikeep.len()))
}

/// Puts the exported instances before the internal ones, as the format requires (instances added to a document go at
/// its end), each group in its own order; remaps every pointer. -> whether any instance moved. An error when an
/// instance points at an instance the document does not have.
pub fn order_exported_first(doc: &mut Document) -> Result<bool> {
    let (exported, internal): (Vec<usize>, Vec<usize>) =
        (0..doc.instances.len()).partition(|&k| doc.instances.get(k).is_some_and(|i| i.exported));
    let order: Vec<usize> = exported.into_iter().chain(internal).collect();
    if order.iter().enumerate().all(|(n, &k)| n == k) {
        return Ok(false);
    }
    rearrange(doc, &order)?;
    Ok(true)
}

/// Keeps the instances `order` lists, in that order, and remaps the internal pointers of the kept ones (each must
/// point at a kept instance; an error when one does not, or `order` names an instance the document does not have).
fn rearrange(doc: &mut Document, order: &[usize]) -> Result<()> {
    let imap: BTreeMap<usize, usize> = order.iter().enumerate().map(|(n, &o)| (o, n)).collect();
    remap_pointers(doc, order, "instance", &|p| match p {
        Pointer::Internal(i) => imap.get(i).map(|&n| Pointer::Internal(n)),
        other => Some(*other),
    })?;
    let mut old = std::mem::take(&mut doc.instances);
    let mut moved = Vec::with_capacity(order.len());
    for &k in order {
        let Some(slot) = old.get_mut(k) else {
            // the document is left as it was found
            doc.instances = old;
            return Err(bad(W, k, "an instance to keep that the document does not have"));
        };
        moved.push(std::mem::replace(slot, placeholder()));
    }
    doc.instances = moved;
    Ok(())
}

fn placeholder() -> Instance {
    Instance {
        fixup_type: 0,
        descriptor: None,
        data_offset: 0,
        exported: false,
        guid: [0; 16],
        object: None,
        raw_image: Vec::new(),
    }
}

/// The field descriptors of `doc`'s type `t` by name, its bases' included.
fn fields_by_name(doc: &Document, t: usize) -> Result<BTreeMap<String, usize>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![t];
    let mut depth = 0usize;
    while let Some(t) = stack.pop() {
        depth = depth.saturating_add(1);
        if depth > 128 {
            return Err(bad(W, 0, "types nest too deeply"));
        }
        let ty = doc
            .types
            .get(t)
            .ok_or_else(|| bad(W, 0, "type descriptor is invalid"))?;
        let first = usize::try_from(ty.field_index).map_err(|_| bad(W, 0, "field range is invalid"))?;
        for di in first..first.saturating_add(usize::from(ty.field_count)) {
            let f = doc
                .fields
                .get(di)
                .ok_or_else(|| bad(W, di, format!("field {di} of {}", doc.fields.len())))?;
            if f.ty() == FieldType::Inherited {
                stack.push(usize::from(f.class_ref));
            } else {
                out.entry(f.name.clone()).or_insert(di);
            }
        }
    }
    Ok(out)
}

/// Carries values from document `from` into document `to`: the instances they point at come along (an exported one
/// only once, by GUID), imports are shared, and types and fields are found in `to` by name.
#[derive(Debug)]
pub struct Carrier<'a> {
    from: &'a Document,
    /// The document values are carried into.
    pub to: &'a mut Document,
    type_by_name: BTreeMap<String, usize>,
    instance_by_guid: BTreeMap<[u8; 16], usize>,
    carried_internal: BTreeMap<usize, usize>,
    fields: BTreeMap<usize, BTreeMap<String, usize>>,
    /// How many instances have been added to `to`.
    pub instances: usize,
    /// How many carried exported instances were given a new object id.
    pub renumbered: usize,
}

impl<'a> Carrier<'a> {
    /// A carrier from `from` into `to`.
    pub fn new(from: &'a Document, to: &'a mut Document) -> Self {
        let mut type_by_name = BTreeMap::new();
        for (i, t) in to.types.iter().enumerate() {
            type_by_name.entry(t.name.clone()).or_insert(i);
        }
        let mut instance_by_guid = BTreeMap::new();
        for (i, inst) in to.instances.iter().enumerate() {
            instance_by_guid.entry(inst.guid).or_insert(i);
        }
        Carrier {
            from,
            to,
            type_by_name,
            instance_by_guid,
            carried_internal: BTreeMap::new(),
            fields: BTreeMap::new(),
            instances: 0,
            renumbered: 0,
        }
    }

    fn ty(&self, descriptor: usize) -> Result<usize> {
        let name = &self
            .from
            .types
            .get(descriptor)
            .ok_or_else(|| bad(W, 0, "type descriptor is out of range"))?
            .name;
        self.type_by_name
            .get(name)
            .copied()
            .ok_or_else(|| bad(W, 0, format!("needs a type the target does not define: {name}")))
    }

    /// A type reference of `from` as one of `to`: the type it names (by GUID and signature, as the document's type
    /// list holds it) gets a slot in the target's list when it has none, and the reference points at that slot.
    fn type_ref(&mut self, t: TypeRef) -> Result<TypeRef> {
        let Some(slot) = t.fixup_type else {
            return Ok(t);
        };
        let (Some(&guid), Some(&signature)) = (
            self.from.fixup_type_guids.get(slot),
            self.from.fixup_type_signatures.get(slot),
        ) else {
            return Err(bad(
                W,
                slot,
                "type reference names a type the source does not list with a signature",
            ));
        };
        let to_slot = usize::from(ensure_fixup_slot(self.to, guid, signature)?);
        t.with_fixup_type(to_slot)
            .ok_or_else(|| bad(W, to_slot, "type slot does not fit a type reference"))
    }

    fn instance(&mut self, index: usize) -> Result<usize> {
        let from = self.from;
        let src = from
            .instances
            .get(index)
            .ok_or_else(|| bad(W, 0, "pointer is out of range"))?;
        if src.exported {
            if let Some(&at) = self.instance_by_guid.get(&src.guid) {
                return Ok(at);
            }
        } else if let Some(&at) = self.carried_internal.get(&index) {
            return Ok(at);
        }
        let obj = src
            .object
            .as_ref()
            .ok_or_else(|| bad(W, 0, "instance has no parsed object"))?;
        let d = self.ty(src.descriptor.ok_or_else(|| bad(W, 0, "instance has no type"))?)?;
        let fixup = ensure_fixup_type(self.to, d)?;
        self.to.instances.push(Instance {
            fixup_type: fixup,
            descriptor: Some(d),
            data_offset: 0,
            exported: src.exported,
            guid: src.guid,
            object: Some(Object {
                descriptor: d,
                fields: Vec::new(),
            }),
            raw_image: src.raw_image.clone(),
        });
        let at = self.to.instances.len().saturating_sub(1);
        if src.exported {
            self.instance_by_guid.entry(src.guid).or_insert(at);
        } else {
            self.carried_internal.insert(index, at);
        }
        let mut copied = self.object(obj)?;
        if src.exported && renumber(&mut copied, &src.guid, from) {
            self.renumbered = self.renumbered.saturating_add(1);
        }
        if let Some(o) = self.to.instances.get_mut(at).and_then(|i| i.object.as_mut()) {
            o.fields = copied.fields;
        }
        self.instances = self.instances.saturating_add(1);
        Ok(at)
    }

    fn imported(&mut self, index: usize) -> Result<usize> {
        let r = *self
            .from
            .imports
            .get(index)
            .ok_or_else(|| bad(W, 0, "import pointer is out of range"))?;
        if let Some(at) = self.to.imports.iter().position(|x| *x == r) {
            return Ok(at);
        }
        self.to.imports.push(r);
        Ok(self.to.imports.len().saturating_sub(1))
    }

    /// `src` (an object of `from`) as an object of `to`, with what it points at carried along.
    pub fn object(&mut self, src: &Object) -> Result<Object> {
        let t = self.ty(src.descriptor)?;
        if !self.fields.contains_key(&t) {
            let m = fields_by_name(self.to, t)?;
            self.fields.insert(t, m);
        }

        let mut out = Object {
            descriptor: t,
            fields: Vec::with_capacity(src.fields.len()),
        };
        for f in &src.fields {
            let descriptor = self
                .fields
                .get(&t)
                .and_then(|m| m.get(&f.name))
                .copied()
                .ok_or_else(|| {
                    let type_name = self.to.types.get(t).map_or("?", |ty| ty.name.as_str());
                    bad(W, 0, format!("the target's {type_name} has no field {}", f.name))
                })?;
            out.fields.push(FieldValue {
                descriptor,
                name: f.name.clone(),
                value: self.value(&f.value)?,
            });
        }
        Ok(out)
    }

    /// `v` (a value of `from`) as a value of `to`.
    pub fn value(&mut self, v: &Value) -> Result<Value> {
        Ok(match v {
            Value::Pointer(Pointer::Internal(i)) => Value::Pointer(Pointer::Internal(self.instance(*i)?)),
            Value::Pointer(Pointer::External(i)) => Value::Pointer(Pointer::External(self.imported(*i)?)),
            Value::Object(o) => Value::Object(Box::new(self.object(o)?)),
            Value::Array(items, h) => {
                Value::Array(items.iter().map(|x| self.value(x)).collect::<Result<_>>()?, *h)
            }
            Value::TypeRef(t) if !t.primitive => Value::TypeRef(self.type_ref(*t)?),
            Value::Boxed(_) => return Err(bad(W, 0, "cannot carry a boxed value yet")),
            other => other.clone(),
        })
    }
}

/// The object id an instance's GUID carries: its first four bytes.
pub(super) fn object_id(guid: &[u8; 16]) -> u32 {
    guid.first_chunk::<4>().map_or(0, |id| u32::from_le_bytes(*id))
}

/// The 32-bit `Flags` word a value holds (an `Int` holds it sign-extended); `None` for a value that is no such word.
pub(super) fn flags_word(v: &Value) -> Option<u32> {
    match *v {
        Value::Int(i) => i32::try_from(i).ok().map(i32::cast_unsigned),
        Value::UInt(u) => u32::try_from(u).ok(),
        _ => None,
    }
}

/// An exported instance carried from another document gets the object id of its own GUID in its `Flags`, when the
/// id it had named one of the source's exported instances. -> whether it changed
fn renumber(obj: &mut Object, guid: &[u8; 16], source: &Document) -> bool {
    let Some(f) = obj.fields.iter_mut().find(|f| f.name == "Flags") else {
        return false;
    };
    let Some(current) = flags_word(&f.value) else {
        return false;
    };
    let new = (current & !OBJECT_ID_MASK) | (object_id(guid) & OBJECT_ID_MASK);
    if new == current {
        return false;
    }
    let cloned = source
        .instances
        .iter()
        .any(|i| i.exported && (object_id(&i.guid) & OBJECT_ID_MASK) == (current & OBJECT_ID_MASK));
    if !cloned {
        return false;
    }
    f.value = match f.value {
        Value::Int(_) => Value::Int(i64::from(new.cast_signed())),
        _ => Value::UInt(u64::from(new)),
    };
    true
}
