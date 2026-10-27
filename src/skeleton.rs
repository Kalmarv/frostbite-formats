//! A skeleton asset (`AntSkeletonAsset`, the EBX documents `animation/dingo/*_skeleton`) read into its joints
//! ([`read`]): facts of the hierarchy only.
//!
//! The asset's `Joints` array holds, for each joint, the offset of its name in the asset's `NameTable` (a table of
//! zero-terminated names), the FNV-1 hash of that name (`NameHash`), its parent, first child and next sibling joint
//! (-1 for none) and a type. Every joint's hash is checked against its name and every parent, child and sibling
//! against the joint count, with no cycle through parents, as the reader's refusals. The asset states no rest
//! transforms: those are in the rig's default pose, keyed by degree-of-freedom ids whose naming is not read, so a
//! joint here has none.

use crate::ebx::{Document, Object, Value};
use crate::{Error, Result};

/// The root type of the skeleton asset document.
pub const ROOT: &str = "AntSkeletonAsset";

/// One joint of a skeleton asset, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkeletonJoint {
    /// The joint's name, read from the asset's `NameTable` at the joint's `Name` offset.
    pub name: String,
    /// The FNV-1 hash of the name, as stored (checked).
    pub name_hash: u32,
    /// Index of the parent joint; `None` for a root (stored as -1). Always below the joint count.
    pub parent: Option<u32>,
    /// Index of the first child joint; `None` for a leaf (stored as -1).
    pub child: Option<u32>,
    /// Index of the next sibling joint; `None` for the last (stored as -1).
    pub sibling: Option<u32>,
    /// The joint's `Type`, as stored.
    pub kind: i64,
}

/// FNV-1 (32 bit) of `name`, the hash the asset stores of a joint's name.
pub fn name_hash(name: &str) -> u32 {
    name.bytes()
        .fold(0x811c_9dc5u32, |h, b| h.wrapping_mul(0x0100_0193) ^ u32::from(b))
}

fn bad(detail: impl Into<String>) -> Error {
    Error::Format {
        what: "skeleton asset",
        offset: 0,
        detail: detail.into(),
    }
}

/// A hierarchy error at joint `i`.
fn joint_error(i: usize, detail: String) -> Error {
    Error::Format {
        what: "skeleton asset",
        offset: i,
        detail: format!("joint {i}: {detail}"),
    }
}

fn int(v: &Value, what: &str) -> Result<i64> {
    match v {
        Value::Int(i) => Ok(*i),
        Value::UInt(u) => i64::try_from(*u).map_err(|_| bad(format!("{what} {u} out of range"))),
        other => Err(bad(format!("{what} is {other:?}, not an integer"))),
    }
}

fn array<'a>(o: &'a Object, field: &str) -> Result<&'a [Value]> {
    match o.field(field) {
        Some(Value::Array(v, _)) => Ok(v),
        _ => Err(bad(format!("no {field} array"))),
    }
}

/// The joints of the document if it is a skeleton asset, `None` if its root type is another; a skeleton asset that
/// does not hold to the module's checks is an error naming the first that fails.
pub fn read(doc: &Document) -> Result<Option<(String, Vec<SkeletonJoint>)>> {
    if doc.root_type != ROOT {
        return Ok(None);
    }
    let root = doc
        .instances
        .first()
        .and_then(|i| i.object.as_ref())
        .ok_or_else(|| bad("no root instance"))?;
    let name = match root.field("Name") {
        Some(Value::Str(s)) => String::from_utf8_lossy(s).into_owned(),
        _ => return Err(bad("no Name")),
    };
    let table: Vec<u8> = array(root, "NameTable")?
        .iter()
        .map(|v| int(v, "NameTable byte").and_then(|b| u8::try_from(b).map_err(|_| bad("NameTable byte"))))
        .collect::<Result<_>>()?;
    let stored = array(root, "Joints")?;
    let count = stored.len();
    let link = |o: &Object, field: &str, joint: usize| -> Result<Option<u32>> {
        let at = int(
            o.field(field)
                .ok_or_else(|| bad(format!("joint {joint}: no {field}")))?,
            field,
        )?;
        match at {
            -1 => Ok(None),
            0.. => match u32::try_from(at) {
                Ok(joint_at) if usize::try_from(at).is_ok_and(|a| a < count) => Ok(Some(joint_at)),
                _ => Err(bad(format!("joint {joint}: {field} {at} of {count} joints"))),
            },
            _ => Err(bad(format!("joint {joint}: {field} {at} of {count} joints"))),
        }
    };
    let mut joints = Vec::with_capacity(count);
    for (i, v) in stored.iter().enumerate() {
        let Value::Object(o) = v else {
            return Err(bad(format!("joint {i} is not an object")));
        };
        let at = usize::try_from(int(
            o.field("Name")
                .ok_or_else(|| bad(format!("joint {i}: no Name")))?,
            "Name",
        )?)
        .map_err(|_| bad(format!("joint {i}: name offset")))?;
        let mut parts = table.get(at..).unwrap_or_default().splitn(2, |&b| b == 0);
        let joint_name = match (parts.next(), parts.next()) {
            (Some(name), Some(_)) => String::from_utf8_lossy(name).into_owned(),
            _ => {
                return Err(bad(format!(
                    "joint {i}: no zero-terminated name at {at} of a table of {}",
                    table.len()
                )));
            }
        };
        let hash = u32::try_from(int(
            o.field("NameHash")
                .ok_or_else(|| bad(format!("joint {i}: no NameHash")))?,
            "NameHash",
        )?)
        .map_err(|_| bad(format!("joint {i}: NameHash")))?;
        if hash != name_hash(&joint_name) {
            return Err(bad(format!(
                "joint {i} {joint_name:?}: hash {hash:#x}, not the FNV-1 of its name"
            )));
        }
        joints.push(SkeletonJoint {
            name: joint_name,
            name_hash: hash,
            parent: link(o, "Parent", i)?,
            child: link(o, "Child", i)?,
            sibling: link(o, "Sibling", i)?,
            kind: int(
                o.field("Type")
                    .ok_or_else(|| bad(format!("joint {i}: no Type")))?,
                "Type",
            )?,
        });
    }
    verify_hierarchy(&joints)?;
    Ok(Some((name, joints)))
}

/// No cycle through the parents, and every parent a joint of the skeleton. The error's offset is the index of the joint
/// whose parents fail (the joints are values, not bytes).
pub fn verify_hierarchy(joints: &[SkeletonJoint]) -> Result<()> {
    for (i, joint) in joints.iter().enumerate() {
        let mut at = joint.parent;
        let mut steps = 0usize;
        while let Some(p) = at {
            steps = steps.saturating_add(1);
            if steps > joints.len() {
                return Err(joint_error(
                    i,
                    "the parents lead back to a joint already passed".into(),
                ));
            }
            at = joints
                .get(p as usize)
                .ok_or_else(|| {
                    joint_error(i, format!("parent {p} is not one of the {} joints", joints.len()))
                })?
                .parent;
        }
    }
    Ok(())
}
