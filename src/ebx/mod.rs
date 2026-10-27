//! Frostbite EBX documents: RIFF `EBX\0`/`EBXS` files holding the EBXD payload, EFIX fixups, EBXX array and
//! boxed-value records, and a REFL or RFL2 reflection table. [`read`] turns one into typed values, [`write()`] lays a
//! document out again, [`carry`] moves values (and what they point at) from one document into another, and [`remap`]
//! gives a document's identities new GUIDs (a clone).
//!
//! This is a port of ReSkate's EBX reader, writer and merge (`Engine/Resource/ebx_document.cpp`,
//! `ebx_writer.cpp` and `ebx_merge.cpp`; github.com/Dingo-Shenanigans/ReSkate, GPL-3.0-or-later), which is the
//! reference for the choices the format leaves open: the donor reflection table is carried as stored, boxed values
//! travel as their bytes plus what those bytes point at, and a new array takes the hash of an array of its field. The
//! port adds bounds checks and strict validation where the C++ trusts its input. A byte comparison with that reference
//! writer therefore measures agreement with the port's source; independent evidence comes from stock documents, whose
//! values must survive a rewrite.
//!
//! Strings are bytes as stored (Frostbite strings are not guaranteed UTF-8). Offsets named `*_offset` are relative to
//! the start of the EBXD payload (`data_start`, 16-aligned) unless they say otherwise.
//!
//! A document read and written again keeps its values; the bytes come back exactly when the document already has the
//! writer's layout (a document the writer produced, or one laid out the same way).

pub mod carry;
mod decode;
mod encode;
pub mod remap;

pub use decode::{read, read_file_guid};
pub use encode::write;

/// `FieldType`: bits 4..9 of an encoded type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FieldType {
    /// Not a field of its own: the type's base class, whose fields come first; `class_ref` names it.
    Inherited = 0x00,
    /// A database object (read as nothing, written as zero bytes).
    DbObject = 0x01,
    /// A nested structure stored inline, of the type `class_ref` names.
    Structure = 0x02,
    /// A reference to another instance of the document or to an import; see [`Pointer`].
    Pointer = 0x03,
    /// An array: a 4-byte displacement to the elements, with the count stored just before them.
    Array = 0x04,
    /// A fixed-length array (read as nothing, written as zero bytes).
    FixedArray = 0x05,
    /// A 32-byte inline string, zero padded.
    String = 0x06,
    /// A string stored elsewhere in the data, reached by a 4-byte displacement; -1 is the empty string.
    CString = 0x07,
    /// An enumeration, a signed 32-bit value.
    Enumeration = 0x08,
    /// A file reference: a string by displacement, then 4 more bytes.
    FileRef = 0x09,
    /// A boolean, one byte.
    Boolean = 0x0A,
    /// A signed 8-bit integer.
    Int8 = 0x0B,
    /// An unsigned 8-bit integer.
    UInt8 = 0x0C,
    /// A signed 16-bit integer.
    Int16 = 0x0D,
    /// An unsigned 16-bit integer.
    UInt16 = 0x0E,
    /// A signed 32-bit integer.
    Int32 = 0x0F,
    /// An unsigned 32-bit integer.
    UInt32 = 0x10,
    /// A signed 64-bit integer.
    Int64 = 0x11,
    /// An unsigned 64-bit integer.
    UInt64 = 0x12,
    /// A 32-bit float.
    Float32 = 0x13,
    /// A 64-bit float.
    Float64 = 0x14,
    /// A 16-byte GUID.
    Guid = 0x15,
    /// A 20-byte SHA-1.
    Sha1 = 0x16,
    /// A 64-bit resource reference.
    ResourceRef = 0x17,
    /// A function reference (read as nothing, written as zero bytes).
    Function = 0x18,
    /// A type reference: 4 bytes naming a primitive type or a reflected type, then 4 more bytes.
    TypeRef = 0x19,
    /// A reference to a boxed value in the boxed-value section; see [`BoxedRef`].
    BoxedValueRef = 0x1A,
    /// An interface reference (read as nothing, written as zero bytes).
    Interface = 0x1B,
    /// A delegate, read like a type reference.
    Delegate = 0x1C,
    /// 0x1D..0x1F: not a type ReSkate knows; read as nothing, written as zero bytes.
    Unknown = 0x1F,
}

impl FieldType {
    /// The type for the five-bit code `v`; a code that is not one of the known types is [`FieldType::Unknown`].
    pub fn from_bits(v: u16) -> FieldType {
        use FieldType::*;
        const ALL: [FieldType; 29] = [
            Inherited,
            DbObject,
            Structure,
            Pointer,
            Array,
            FixedArray,
            String,
            CString,
            Enumeration,
            FileRef,
            Boolean,
            Int8,
            UInt8,
            Int16,
            UInt16,
            Int32,
            UInt32,
            Int64,
            UInt64,
            Float32,
            Float64,
            Guid,
            Sha1,
            ResourceRef,
            Function,
            TypeRef,
            BoxedValueRef,
            Interface,
            Delegate,
        ];
        ALL.get(usize::from(v)).copied().unwrap_or(Unknown)
    }
}

/// The category nibble of an encoded type (`FieldCategory`); only `Array` (4) changes how a field is read.
const CATEGORY_ARRAY: u16 = 4;

/// A field of a reflected type: its name, encoded type and where it lies in an instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDescriptor {
    /// The field's name, looked up from the reflection table's strings.
    pub name: String,
    /// The name hash as stored: in a document with hashed names (RFL2) the key of the name table, otherwise the name's offset in the reflection strings.
    pub name_hash: u32,
    /// The encoded type as ReSkate keeps it: the stored u16 shifted right by one.
    pub encoded_type: u16,
    /// The type slot of a structure, enumeration, inherited or array-of-structure field (index into [`Document::types`]); not meaningful for other types.
    pub class_ref: u16,
    /// Byte offset of the field from the start of its instance.
    pub data_offset: u32,
}

impl FieldDescriptor {
    /// The field's [`FieldType`].
    pub fn ty(&self) -> FieldType {
        FieldType::from_bits((self.encoded_type >> 4) & 0x1F)
    }
    /// Whether the field is an array.
    pub fn is_array(&self) -> bool {
        self.encoded_type & 0x0F == CATEGORY_ARRAY
    }
}

/// A reflected type: its name, size, alignment, GUID, signature and the run of [`Document::fields`] it owns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeDescriptor {
    /// The type's name, looked up from the reflection table's strings.
    pub name: String,
    /// The name hash as stored; see [`FieldDescriptor::name_hash`].
    pub name_hash: u32,
    /// Index into [`Document::fields`] of the type's first field; the type owns `field_count` consecutive fields.
    pub field_index: i32,
    /// Number of fields the type owns.
    pub field_count: u16,
    /// The encoded type as ReSkate keeps it: the stored u16 shifted right by one.
    pub encoded_type: u16,
    /// Size in bytes of an instance of the type.
    pub size: u16,
    /// Alignment in bytes; the reader refuses a value over 255.
    pub alignment: u16,
    /// The type's GUID, taken from the reflection table's identifier list in step with the types.
    pub guid: [u8; 16],
    /// The type's signature, taken from the same list; with the GUID it ties an instance to its type.
    pub signature: u32,
}

/// An import: the file GUID of another document and the GUID of the instance in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportReference {
    /// GUID of the imported document (the target's file GUID).
    pub file_guid: [u8; 16],
    /// GUID of the instance within that document.
    pub class_guid: [u8; 16],
}

/// An EBXX array record: where an array's data starts, its length, its hash and element type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArrayRecord {
    /// Offset of the array's elements from the start of the EBXD payload.
    pub offset: u32,
    /// Number of elements.
    pub count: u32,
    /// The array hash, kept so that an array read and written again keeps it.
    pub hash: u32,
    /// As stored (not shifted).
    pub encoded_type: u16,
    /// For struct elements, the EFIX type slot; else 0xFFFF.
    pub class_ref: u16,
}

/// One RFL2 group record: an array type the document uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReflectionGroup {
    /// Hash that names the array type.
    pub hash: u32,
    /// Number of array types in the group.
    pub count: u32,
    /// Index of the group's first entry.
    pub first: u32,
}

/// An EBXX boxed-value record: where a boxed value starts, its hash and type, and its raw bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoxedValueRecord {
    /// Offset of the boxed value from the start of the EBXD payload.
    pub offset: u32,
    /// Count as stored in the record.
    pub count: u32,
    /// The record's hash.
    pub hash: u32,
    /// The encoded type as stored (not shifted).
    pub encoded_type: u16,
    /// For struct values, the EFIX type slot; else 0xFFFF.
    pub class_ref: u16,
    /// The record's bytes, up to the next record or the string section.
    pub raw: Vec<u8>,
}

/// An object stored in the document, with its type, its GUID if it is exported, and its decoded values.
#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    /// Index of the instance's type in the EFIX type list ([`Document::fixup_type_guids`]).
    pub fixup_type: u16,
    /// Index into [`Document::types`]; `None` when its EFIX type has no reflection entry (then it is not read).
    pub descriptor: Option<usize>,
    /// Offset of the instance from the start of the EBXD payload.
    pub data_offset: u32,
    /// Whether the instance is one of the document's exported instances (the first ones the EFIX table lists), which carry a GUID in the 16 bytes before them.
    pub exported: bool,
    /// The instance's GUID if it is exported; zero otherwise.
    pub guid: [u8; 16],
    /// The instance's values; `None` when its type is unknown ([`Instance::descriptor`] is `None`).
    pub object: Option<Object>,
    /// The instance's bytes as read (its type's size): padding and unread fields are written back from here.
    pub raw_image: Vec<u8>,
}

/// What a pointer field holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pointer {
    /// A null pointer.
    Null,
    /// An instance of this document, by index.
    Internal(usize),
    /// An import, by index.
    External(usize),
}

/// A type reference value: a primitive type or a type of the document's type list.
///
/// A reference to a type of the document is `slot << 2 | low bits` in [`TypeRef::encoded`], where `slot` indexes the
/// EFIX type list ([`Document::fixup_type_guids`]), the list the instances' types are named from too. It is not an index
/// into the reflection table: [`Document::type_ref_target`] finds the reflected type a slot names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypeRef {
    /// Whether the reference names a primitive type rather than a type of the document.
    pub primitive: bool,
    /// The primitive type, when `primitive` is set.
    pub primitive_type: Option<FieldType>,
    /// Index into [`Document::fixup_type_guids`], when the reference names a type of the document (`encoded >> 2`).
    pub fixup_type: Option<usize>,
    /// The reference as stored (u32): the writer emits this, so a changed [`TypeRef::fixup_type`] takes effect through
    /// [`TypeRef::with_fixup_type`]. The low two bits are kept as stored; their meaning is not known.
    pub encoded: u32,
}

impl TypeRef {
    /// This reference with its EFIX type list slot replaced and the low two bits of `encoded` kept; `None` when the
    /// slot does not fit the encoding or the reference is empty or primitive.
    pub fn with_fixup_type(self, slot: usize) -> Option<TypeRef> {
        if self.primitive || self.fixup_type.is_none() {
            return None;
        }
        let shifted = u32::try_from(slot).ok().filter(|&s| s < 1 << 29)? << 2;
        Some(TypeRef {
            fixup_type: Some(slot),
            encoded: shifted | (self.encoded & 3),
            ..self
        })
    }
}

/// A reference to a boxed value in the document's boxed-value section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoxedRef {
    /// The encoded type of the boxed value as stored (u32).
    pub encoded_type: u32,
    /// Payload offset of the boxed value, or -1.
    pub data_offset: i64,
    /// The displacement as stored: from 8 bytes past the field to the boxed value.
    pub relative_offset: i64,
}

/// A field's value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A field type that is read as nothing (database objects, functions and so on).
    Nothing,
    /// A boolean.
    Bool(bool),
    /// A signed integer of 8, 16, 32 or 64 bits, or an enumeration.
    Int(i64),
    /// An unsigned integer of 8, 16, 32 or 64 bits.
    UInt(u64),
    /// A 32-bit or 64-bit float, widened to `f64`. A NaN keeps the bits it was stored with (a 32-bit NaN widens by
    /// shifting its payload, quiet or signalling) and is written back with them.
    Float(f64),
    /// String, CString and FileRef values, as stored.
    Str(Vec<u8>),
    /// A 16-byte GUID.
    Guid([u8; 16]),
    /// A 20-byte SHA-1.
    Sha1([u8; 20]),
    /// A 64-bit resource reference.
    Resource(u64),
    /// A pointer to an instance or an import.
    Pointer(Pointer),
    /// A type reference.
    TypeRef(TypeRef),
    /// A boxed-value reference.
    Boxed(BoxedRef),
    /// A nested structure.
    Object(Box<Object>),
    /// An array's elements, and the hash of the record it was read from (a new array has none).
    Array(Vec<Value>, Option<u32>),
}

/// A field of an [`Object`] with its value.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldValue {
    /// Index into [`Document::fields`].
    pub descriptor: usize,
    /// The field's name.
    pub name: String,
    /// The field's value.
    pub value: Value,
}

/// A structure: its type and its fields' values, in stored order.
#[derive(Clone, Debug, PartialEq)]
pub struct Object {
    /// Index into [`Document::types`] of the object's type.
    pub descriptor: usize,
    /// The values of the type's fields, those of its base types first.
    pub fields: Vec<FieldValue>,
}

impl Object {
    /// The field named `name` (ASCII case-insensitive, as ReSkate looks fields up).
    pub fn field(&self, name: &str) -> Option<&Value> {
        self.fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(name))
            .map(|f| &f.value)
    }
    /// The field named `name`, mutably.
    pub fn field_mut(&mut self, name: &str) -> Option<&mut Value> {
        self.fields
            .iter_mut()
            .find(|f| f.name.eq_ignore_ascii_case(name))
            .map(|f| &mut f.value)
    }
}

/// An EBX document read by [`read`]: its instances with typed values, together with the reflection data, fixup
/// tables and payload that [`write()`] needs to lay it out again.
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    /// The document's own GUID (the first 16 bytes of the EFIX chunk); other documents import it by this.
    pub file_guid: [u8; 16],
    /// Name of the first instance's type.
    pub root_type: String,
    /// Absolute offsets of the EBXD payload in the bytes read.
    pub data_start: usize,
    /// Absolute offset of the end of the EBXD payload in the bytes read.
    pub data_end: usize,
    /// Offset of the EBXX array section, from the start of the payload (EFIX).
    pub arrays_offset: u32,
    /// Offset of the boxed-value section, from the start of the payload (EFIX).
    pub boxed_values_offset: u32,
    /// Offset of the string section, from the start of the payload (EFIX).
    pub strings_offset: u32,
    /// Whether the RIFF form is `EBXS` rather than `EBX`.
    pub serialized_layout: bool,
    /// Whether the reflection chunk is RFL2 (names looked up by hash) rather than REFL.
    pub hashed_reflection_names: bool,
    /// The reflection chunk (REFL or RFL2) as stored.
    pub reflection_data: Vec<u8>,
    /// The GUIDs of the types the instances use (EFIX), indexed by [`Instance::fixup_type`].
    pub fixup_type_guids: Vec<[u8; 16]>,
    /// The signatures that go with `fixup_type_guids`; there may be fewer than GUIDs.
    pub fixup_type_signatures: Vec<u32>,
    /// The offsets of boxed-value references (EFIX), if the chunk carries them.
    pub boxed_reference_offsets: Vec<u32>,
    /// The reflected types.
    pub types: Vec<TypeDescriptor>,
    /// The reflected fields; each type owns a run of them.
    pub fields: Vec<FieldDescriptor>,
    /// The instances in EFIX order, the exported ones first.
    pub instances: Vec<Instance>,
    /// The documents this one imports from.
    pub imports: Vec<ImportReference>,
    /// The EBXX array records.
    pub arrays: Vec<ArrayRecord>,
    /// The EBXX boxed-value records, sorted by offset.
    pub boxed_values: Vec<BoxedValueRecord>,
    /// Payload offsets of the pointer fields (EFIX).
    pub pointer_offsets: Vec<u32>,
    /// Payload offsets of the resource-reference fields (EFIX).
    pub resource_ref_offsets: Vec<u32>,
    /// Payload offsets of the import pointers (EFIX).
    pub import_offsets: Vec<u32>,
    /// Payload offsets of the type-reference fields (EFIX).
    pub type_info_offsets: Vec<u32>,
    /// Payload offsets of the boxed-value fields found while reading values.
    pub boxed_value_slots: Vec<u32>,
    /// The EBXD bytes as read (from `data_start`): what boxed values point at is carried from here.
    pub payload: Vec<u8>,
    /// The RFL2 array-type groups.
    pub reflection_groups: Vec<ReflectionGroup>,
}

impl Document {
    /// The reflected type ([`Document::types`] index) the EFIX type list's `slot` names, by GUID and signature (the
    /// GUID alone when the list has no signature for the slot); `None` when the slot is past the list or the document
    /// reflects no such type (the list names GUIDs the reflection table lacks).
    pub fn fixup_type_target(&self, slot: usize) -> Option<usize> {
        let guid = self.fixup_type_guids.get(slot)?;
        let signature = self.fixup_type_signatures.get(slot);
        self.types
            .iter()
            .position(|t| t.guid == *guid && signature.is_none_or(|s| t.signature == *s))
    }

    /// The reflected type ([`Document::types`] index) a type reference names; `None` for an empty or primitive
    /// reference and for one whose EFIX slot reflects no type.
    pub fn type_ref_target(&self, t: &TypeRef) -> Option<usize> {
        self.fixup_type_target(t.fixup_type?)
    }

    /// The type name of instance `i`.
    pub fn type_name(&self, i: usize) -> Option<&str> {
        let d = self.instances.get(i)?.descriptor?;
        self.types.get(d).map(|t| t.name.as_str())
    }
}
