//! The field-level readings of EBX documents, shader block depots and bundle manifests, checked over every TOC of the
//! skate. install (`installs.skate/Data`, `tools.oodle9`; about half a minute in release, the TOCs read in parallel):
//!
//! - every distinct EBX payload reads, is written again without a refusal (the 213 stock documents holding a NaN or an
//!   infinity included) and reads back to the same values, floats compared by bits;
//! - the two 64-bit integer codes: a field whose name in the game's own reflection says `Int64` is code 0x11 and one
//!   that says `Uint64` or `U64` is code 0x12, and each reads as the matching `Value` variant;
//! - type references name a slot of the EFIX type list (always inside it), and a slot whose GUID some document of the
//!   install reflects resolves to a type whose name fits the field (`TraitDataTypeRef` names `...TraitData` types);
//! - every enumeration value is named by `Document::enum_member` but one (the census counted 3,368,510 values over 605
//!   enumeration types, one of them naming no member), and no enumeration type is memberless;
//! - the file GUID -> name table (`GuidNames`, built bundle by bundle) names every document of the install, no GUID gets
//!   two names, and it agrees with the file GUIDs read out of the fully decoded documents;
//! - every shader block of every depot parses to its last byte, the blocks with parameter type `0x566e2dd0` included;
//! - a manifest chunk row's size is its stored range's `lsize`, `loff` is a multiple of 65,536, and `loff + lsize` is
//!   the decoded size of the whole chunk in the TOC's chunk table.

#![allow(clippy::unwrap_used)]

mod inputs;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use frostbite_formats::bundle::Kind;
use frostbite_formats::depot;
use frostbite_formats::ebx::{self, Document, FieldType, Object, TypeRef, Value};
use frostbite_formats::folder::{Folder, Source};
use frostbite_formats::names::GuidNames;
use inputs::{Game, Tool, install_file, tool};
use primase_foundations::compress::oodle::{Oodle, Version};

#[derive(Default)]
struct Tally {
    counts: BTreeMap<&'static str, u64>,
    problems: Vec<String>,
    int64_fields: BTreeSet<String>,
    uint64_fields: BTreeSet<String>,
    /// (field, EFIX GUID) of every type reference to a type of the document, with how many
    refs: BTreeMap<(String, [u8; 16]), u64>,
    /// every reflected type's GUID -> names, over the install
    guid_names: BTreeMap<[u8; 16], BTreeSet<String>>,
    /// the enumeration types (by name) used by a field, and those of them with no member
    enum_types: BTreeSet<String>,
    /// the members (name, value) of every enumeration type used by a field, by type name
    enum_members: BTreeMap<String, Vec<(String, i64)>>,
    memberless_enums: BTreeSet<String>,
    /// every document's file GUID -> its manifest names, as the decoded documents say it
    file_names: BTreeMap<[u8; 16], BTreeSet<String>>,
    /// every import's file GUID, with how many imports name it
    imports: BTreeMap<[u8; 16], u64>,
}

impl Tally {
    fn n(&mut self, k: &'static str) {
        *self.counts.entry(k).or_default() += 1;
    }
    fn problem(&mut self, s: String) {
        if self.problems.len() < 20 {
            self.problems.push(s);
        }
    }
    fn merge(&mut self, o: Tally) {
        for (k, v) in o.counts {
            *self.counts.entry(k).or_default() += v;
        }
        self.problems.extend(o.problems);
        self.int64_fields.extend(o.int64_fields);
        self.uint64_fields.extend(o.uint64_fields);
        for (k, v) in o.refs {
            *self.refs.entry(k).or_default() += v;
        }
        for (k, v) in o.guid_names {
            self.guid_names.entry(k).or_default().extend(v);
        }
        self.enum_types.extend(o.enum_types);
        self.enum_members.extend(o.enum_members);
        self.memberless_enums.extend(o.memberless_enums);
        for (k, v) in o.file_names {
            self.file_names.entry(k).or_default().extend(v);
        }
        for (k, v) in o.imports {
            *self.imports.entry(k).or_default() += v;
        }
    }
}

struct Walk<'a> {
    doc: &'a Document,
    tally: &'a mut Tally,
    name: &'a str,
}

impl Walk<'_> {
    fn object(&mut self, o: &Object) {
        let ty = self.doc.types.get(o.descriptor).map_or("?", |t| t.name.as_str());
        for fv in &o.fields {
            let key = format!("{ty}.{}", fv.name);
            self.value(&key, fv.descriptor, &fv.value);
        }
    }

    fn value(&mut self, key: &str, field: usize, v: &Value) {
        let field_type = self.doc.fields.get(field).map(|f| f.ty());
        match v {
            Value::Object(o) => self.object(o),
            Value::Array(items, _) => items.iter().for_each(|x| self.value(key, field, x)),
            Value::Int(n) if field_type == Some(FieldType::Enumeration) => self.enumeration(field, *n),
            Value::Int(_) | Value::UInt(_) => match (field_type, v) {
                (Some(FieldType::Int64), Value::Int(_)) => self.integer64(key, true),
                (Some(FieldType::UInt64), Value::UInt(_)) => self.integer64(key, false),
                (Some(FieldType::Int64 | FieldType::UInt64), _) => {
                    self.tally.n("64-bit value of the other variant");
                    self.tally.problem(format!("{}: {key} reads as {v:?}", self.name));
                }
                _ => {}
            },
            Value::TypeRef(t) => self.type_ref(key, t),
            _ => {}
        }
    }

    fn enumeration(&mut self, field: usize, value: i64) {
        let doc = self.doc;
        let ty = doc
            .fields
            .get(field)
            .and_then(|f| doc.types.get(usize::from(f.class_ref)));
        let name = ty.map_or("?".to_string(), |t| t.name.clone());
        self.tally.enum_types.insert(name.clone());
        if !self.tally.enum_members.contains_key(&name) {
            let members = doc.enum_members(field).map(|(n, v)| (n.to_string(), v)).collect();
            self.tally.enum_members.insert(name.clone(), members);
        }
        if ty.is_some_and(|t| t.field_count == 0) {
            self.tally.memberless_enums.insert(name.clone());
        }
        self.tally.n("enumeration values");
        match doc.enum_member(field, value) {
            Some(member) => {
                self.tally.n("enumeration values naming a member");
                // the member reads back to the value, and the first member of that value is the one named
                if !doc.enum_members(field).any(|(m, v)| m == member && v == value) {
                    self.tally.n("enumeration members that do not read back");
                }
            }
            None => {
                self.tally.n("enumeration values naming no member");
                self.tally.problem(format!(
                    "{}: enumeration {name} = {value} names no member",
                    self.name
                ));
            }
        }
    }

    fn integer64(&mut self, key: &str, signed: bool) {
        let field = key.rsplit('.').next().unwrap_or(key).to_ascii_lowercase();
        let named_unsigned = field.contains("uint64") || field.contains("u64");
        let named_signed = !named_unsigned && field.contains("int64");
        if (named_unsigned && signed) || (named_signed && !signed) {
            self.tally.n("64-bit field named for the other signedness");
            self.tally.problem(format!(
                "{key} is read as {}",
                if signed { "Int64" } else { "UInt64" }
            ));
        }
        if named_unsigned || named_signed {
            self.tally.n("64-bit fields named for their signedness");
        }
        if signed {
            self.tally.int64_fields.insert(key.to_string());
        } else {
            self.tally.uint64_fields.insert(key.to_string());
        }
    }

    fn type_ref(&mut self, key: &str, t: &TypeRef) {
        if t.encoded == 0 || t.primitive {
            return;
        }
        let doc = self.doc;
        let Some(slot) = t.fixup_type.filter(|&s| s < doc.fixup_type_guids.len()) else {
            self.tally.n("type reference past the EFIX list");
            self.tally
                .problem(format!("{}: {key} names slot {:?}", self.name, t.fixup_type));
            return;
        };
        self.tally.n("type references");
        if t.with_fixup_type(slot) != Some(*t) {
            self.tally
                .n("type reference that with_fixup_type does not give back");
        }
        if let Some(i) = doc.type_ref_target(t) {
            let ty = &doc.types[i];
            if ty.guid != doc.fixup_type_guids[slot] {
                self.tally.n("type reference resolved to another GUID");
            }
            self.tally
                .n("type references resolved by this document's reflection");
        }
        *self
            .tally
            .refs
            .entry((key.to_string(), doc.fixup_type_guids[slot]))
            .or_default() += 1;
        // the slot read as an index into the reflection table instead (what was done before): a different type
        if key.ends_with(".TraitDataTypeRef") {
            self.tally.n("TraitDataTypeRef references");
            if doc
                .types
                .get(slot)
                .is_some_and(|ty| ty.name.ends_with("TraitData"))
            {
                self.tally.n(
                    "TraitDataTypeRef references whose reflection-table entry at the slot is a trait type",
                );
            }
        }
    }
}

/// Values equal, floats by bits; boxed values by their type (the writer lays them out elsewhere).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::Array(x, _), Value::Array(y, _)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Object(x), Value::Object(y)) => same_object(x, y),
        (Value::Boxed(x), Value::Boxed(y)) => x.encoded_type == y.encoded_type,
        _ => a == b,
    }
}

fn same_object(a: &Object, b: &Object) -> bool {
    a.fields.len() == b.fields.len()
        && a.fields
            .iter()
            .zip(&b.fields)
            .all(|(p, q)| p.name == q.name && same(&p.value, &q.value))
}

fn has_non_finite(v: &Value) -> bool {
    match v {
        Value::Float(f) => !f.is_finite(),
        Value::Array(items, _) => items.iter().any(has_non_finite),
        Value::Object(o) => o.fields.iter().any(|f| has_non_finite(&f.value)),
        _ => false,
    }
}

fn document(t: &mut Tally, bytes: &[u8], name: &str) {
    let doc = match ebx::read(bytes) {
        Ok(d) => d,
        Err(e) => {
            t.n("EBX payloads refused by the reader");
            t.problem(format!("{name}: {e}"));
            return;
        }
    };
    t.n("EBX payloads read");
    t.file_names
        .entry(doc.file_guid)
        .or_default()
        .insert(name.to_string());
    for i in &doc.imports {
        *t.imports.entry(i.file_guid).or_default() += 1;
    }
    for ty in &doc.types {
        t.guid_names.entry(ty.guid).or_default().insert(ty.name.clone());
    }
    let mut walk = Walk {
        doc: &doc,
        tally: t,
        name,
    };
    for o in doc.instances.iter().filter_map(|i| i.object.as_ref()) {
        walk.object(o);
    }
    let non_finite = doc
        .instances
        .iter()
        .filter_map(|i| i.object.as_ref())
        .any(|o| o.fields.iter().any(|f| has_non_finite(&f.value)));
    if non_finite {
        t.n("documents holding a NaN or an infinity");
    }
    let written = match ebx::write(&doc) {
        Ok(w) => w,
        Err(e) => {
            t.n("EBX payloads refused by the writer");
            t.problem(format!("{name}: {e}"));
            return;
        }
    };
    let Ok(again) = ebx::read(&written) else {
        t.n("rewritten EBX payloads that do not read");
        t.problem(format!("{name}: the rewrite does not read"));
        return;
    };
    let objects = |d: &Document| d.instances.iter().map(|i| i.object.clone()).collect::<Vec<_>>();
    let (a, b) = (objects(&doc), objects(&again));
    let ok = a.len() == b.len()
        && again.file_guid == doc.file_guid
        && a.iter().zip(&b).all(|(x, y)| match (x, y) {
            (Some(x), Some(y)) => same_object(x, y),
            (None, None) => true,
            _ => false,
        });
    if !ok {
        t.n("rewritten EBX payloads that read back to other values");
        t.problem(format!("{name}: the rewrite reads back differently"));
    } else if non_finite {
        t.n("documents with a NaN or an infinity that read back to the same bits");
    }
}

#[test]
fn field_readings_hold_over_the_install() -> Result<(), Box<dyn std::error::Error>> {
    let (Some(data), Some(dll)) = (install_file(Game::Skate, "Data"), tool(Tool::Oodle9)) else {
        return Ok(());
    };
    let folder = Folder::open(data)?;
    // SAFETY: the configuration names the game's own Oodle DLL as tools.oodle9.
    #[allow(unsafe_code)]
    let oodle = unsafe { Oodle::load(dll, Version::V9) }?;
    let src = Source {
        folder: &folder,
        base: Some(&folder),
        oodle: Some(&oodle),
    };
    let tocs = folder.tocs.clone();
    let seen = Mutex::new(HashSet::new());
    let next = AtomicUsize::new(0);
    let total = Mutex::new(Tally::default());
    let names = Mutex::new((GuidNames::new(), 0usize));
    let threads = std::thread::available_parallelism().map_or(4, usize::from).min(8);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some(toc) = tocs.get(next.fetch_add(1, Ordering::SeqCst)) {
                    let mut t = Tally::default();
                    let listing = match src.listing(toc) {
                        Ok(l) => l,
                        Err(e) => {
                            t.n("TOCs that do not list");
                            t.problem(format!("{toc}: {e}"));
                            total.lock().unwrap().merge(t);
                            continue;
                        }
                    };
                    t.counts.insert("bundles that did not read", listing.unread.len() as u64);
                    let mut local = GuidNames::new();
                    let mut done = std::collections::BTreeSet::new();
                    let mut unreadable = 0;
                    for bundle in &listing.bundles {
                        unreadable += local.add_bundle(&src, bundle, &mut done).unreadable;
                    }
                    {
                        let mut n = names.lock().unwrap();
                        n.0.extend(local);
                        n.1 += unreadable;
                    }
                    for bundle in &listing.bundles {
                        for row in &bundle.manifest.chunks {
                            t.n("manifest chunk rows");
                            if row.size != row.lsize {
                                t.n("chunk rows whose size is not lsize");
                            }
                            if row.loff % 65536 != 0 {
                                t.n("chunk rows whose loff is not a multiple of 65,536");
                            }
                            if row.loff != 0 {
                                match bundle.toc_chunk(&src, &row.guid) {
                                    Ok(Some((_, whole))) if row.chunk_size() == whole as u64 => {
                                        t.n("chunk rows with loff != 0 whose loff + lsize is the whole chunk");
                                    }
                                    other => {
                                        t.n("chunk rows with loff != 0 whose loff + lsize is not the whole chunk");
                                        t.problem(format!("{}: {other:?}", bundle.name));
                                    }
                                }
                            }
                        }
                        for e in bundle.entries() {
                            let key = (e.file.0.encode(), e.file.1, e.file.2);
                            let is_depot = e.kind == Kind::Res && e.resource_type == Some(depot::RTYPE);
                            if !(e.kind == Kind::Ebx || is_depot) || !seen.lock().unwrap().insert(key) {
                                continue;
                            }
                            let Ok(bytes) = src.payload(e.file, e.size as usize) else {
                                t.n("payloads that did not read");
                                continue;
                            };
                            if !is_depot {
                                document(&mut t, &bytes, &e.name);
                                continue;
                            }
                            let meta = bundle.manifest.res[e.index].rmeta;
                            let d = match depot::read(&bytes, &meta) {
                                Ok(d) => d,
                                Err(err) => {
                                    t.n("depots refused");
                                    t.problem(format!("{}: {err}", e.name));
                                    continue;
                                }
                            };
                            t.n("depots read");
                            for block in &d.blocks {
                                match depot::params(&block.data) {
                                    Ok(ps) => {
                                        t.n("shader blocks parsed to their last byte");
                                        if ps.iter().any(|p| p.ty == 0x566e_2dd0) {
                                            t.n("shader blocks holding parameter type 0x566e2dd0");
                                        }
                                    }
                                    Err(err) => {
                                        t.n("shader blocks refused");
                                        t.problem(format!("{}: {err}", e.name));
                                    }
                                }
                            }
                        }
                    }
                    total.lock().unwrap().merge(t);
                }
            });
        }
    });
    let t = total.into_inner().unwrap();
    let (names, names_unreadable) = names.into_inner().unwrap();
    println!("{:#?}", t.counts);
    let count = |k: &str| t.counts.get(k).copied().unwrap_or(0);

    // nothing refused, nothing changed
    for refused in [
        "TOCs that do not list",
        "bundles that did not read",
        "payloads that did not read",
        "EBX payloads refused by the reader",
        "EBX payloads refused by the writer",
        "rewritten EBX payloads that do not read",
        "rewritten EBX payloads that read back to other values",
        "depots refused",
        "shader blocks refused",
    ] {
        assert_eq!(count(refused), 0, "{refused}: {:?}", t.problems);
    }
    assert!(
        count("EBX payloads read") > 300_000,
        "{}",
        count("EBX payloads read")
    );
    // the documents with a NaN or an infinity: all of them rewritten with their bits
    assert!(count("documents holding a NaN or an infinity") > 0);
    assert_eq!(
        count("documents holding a NaN or an infinity"),
        count("documents with a NaN or an infinity that read back to the same bits")
    );
    // the 64-bit codes: the game's field names agree with the reading, over the fields it has
    for k in [
        "64-bit field named for the other signedness",
        "64-bit value of the other variant",
        "type reference past the EFIX list",
        "type reference that with_fixup_type does not give back",
        "type reference resolved to another GUID",
    ] {
        assert_eq!(count(k), 0, "{k}: {:?}", t.problems);
    }
    assert!(count("64-bit fields named for their signedness") > 0);
    assert!(t.int64_fields.contains("CompareInt64EntityData.A"));
    assert!(t.int64_fields.contains("PropertyCastEntityData.Int64Value"));
    assert!(t.uint64_fields.contains("CompareUint64EntityData.A"));
    assert!(t.uint64_fields.contains("ExpressionPortData.PortKey"));
    assert!(
        t.uint64_fields
            .contains("EnvironmentDecalVolumeData.ShaderInstanceKey")
    );
    // type references: a slot of the EFIX list names the type the field is about
    let mut trait_refs = 0u64;
    for ((key, guid), n) in &t.refs {
        if key.ends_with(".TraitDataTypeRef")
            && let Some(names) = t.guid_names.get(guid)
        {
            assert!(
                names.iter().all(|name| name.ends_with("TraitData")),
                "{key} names {names:?}"
            );
            trait_refs += n;
        }
    }
    assert!(trait_refs > 1000, "{trait_refs} trait type references resolved");
    // read as an index into the reflection table, these references would mostly name other types
    let all = count("TraitDataTypeRef references");
    let by_table_index =
        count("TraitDataTypeRef references whose reflection-table entry at the slot is a trait type");
    assert!(
        all > 1000 && by_table_index * 2 < all,
        "{by_table_index} of {all}"
    );
    // enumerations: every value but one names a member of its type, and no type is memberless
    println!(
        "{} enumeration types used, {} memberless; {} documents named by GuidNames ({} conflicts, {names_unreadable} unreadable); {} import file GUIDs",
        t.enum_types.len(),
        t.memberless_enums.len(),
        names.len(),
        names.conflicts().len(),
        t.imports.len()
    );
    for (name, members) in &t.enum_members {
        if [
            "Mesh",
            "Primitive",
            "Instancing",
            "IndexBuffer",
            "SubsetCategory",
            "VertexElement",
        ]
        .iter()
        .any(|k| name.contains(k))
        {
            println!("ENUM {name}: {members:?}");
        }
    }
    assert_eq!(count("enumeration values"), 3_368_510, "{:?}", t.problems);
    assert_eq!(count("enumeration values naming a member"), 3_368_509);
    assert_eq!(count("enumeration values naming no member"), 1);
    assert_eq!(count("enumeration members that do not read back"), 0);
    assert!(t.memberless_enums.is_empty(), "{:?}", t.memberless_enums);
    // the file GUID table: it names the documents the decoded files say it does, with no GUID given two names
    let (mut import_refs, mut resolved_refs) = (0u64, 0u64);
    let (mut resolved_guids, mut unresolved) = (0u64, Vec::new());
    for (guid, n) in &t.imports {
        import_refs += n;
        if names.get(guid).is_some() {
            resolved_refs += n;
            resolved_guids += 1;
        } else if unresolved.len() < 3 {
            unresolved.push(frostbite_formats::hex(guid));
        }
    }
    println!(
        "imports: {import_refs} references to {} file GUIDs; {resolved_refs} references and {resolved_guids} GUIDs named by the table (unnamed e.g. {unresolved:?})",
        t.imports.len()
    );
    // the census of build 20260929: 865,673 references to 147,320 file GUIDs; two GUIDs, one reference each, name a
    // document no bundle of the install holds
    assert_eq!(import_refs, 865_673);
    assert_eq!(resolved_refs, 865_671, "{unresolved:?}");
    assert_eq!(resolved_guids, 147_318);
    assert_eq!(names_unreadable, 0);
    assert!(names.conflicts().is_empty(), "{:?}", names.conflicts());
    assert_eq!(names.len(), t.file_names.len());
    for (guid, expected) in &t.file_names {
        let got = names.get(guid).unwrap_or("");
        assert!(expected.contains(got), "{got} not among {expected:?}");
    }
    // the depots: every block parses, the one-byte parameter type among them
    assert!(count("shader blocks parsed to their last byte") > 60_000);
    assert!(count("shader blocks holding parameter type 0x566e2dd0") > 0);
    // the manifests
    assert!(count("manifest chunk rows") > 200_000);
    assert_eq!(count("chunk rows whose size is not lsize"), 0, "{:?}", t.problems);
    assert_eq!(count("chunk rows whose loff is not a multiple of 65,536"), 0);
    assert_eq!(
        count("chunk rows with loff != 0 whose loff + lsize is not the whole chunk"),
        0,
        "{:?}",
        t.problems
    );
    assert!(count("chunk rows with loff != 0 whose loff + lsize is the whole chunk") > 0);
    Ok(())
}
