//! A corpus-level table from an EBX document's file GUID to its name. A document names another by the file GUID of the
//! target ([`ebx::ImportReference::file_guid`](crate::ebx::ImportReference)), and the GUID alone says nothing; the
//! document's name lies in the manifest of the bundle that holds it, so the table is built from bundles and then
//! answers for any document that refers to a GUID some bundle of the same data folder holds.

use std::collections::{BTreeMap, BTreeSet};

use crate::bundle::Kind;
use crate::folder::Source;
use crate::listing::Bundle;

/// File GUID -> document name (as the manifests state it, lowercase in the base game's data).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GuidNames {
    names: BTreeMap<[u8; 16], String>,
    /// GUIDs two documents of different names claimed: the first name is kept.
    conflicts: Vec<([u8; 16], String, String)>,
}

/// What [`GuidNames::add_bundle`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Added {
    /// EBX documents of the bundle whose file GUID was read.
    pub documents: usize,
    /// Documents whose payload would not read (a missing archive or an Oodle block with no library).
    pub unreadable: usize,
}

impl GuidNames {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the document `name` has the file GUID `guid`. A second name for a GUID (compared ignoring ASCII case:
    /// the same document reached through two bundles repeats its name) is kept in [`GuidNames::conflicts`], the first stays.
    pub fn insert(&mut self, guid: [u8; 16], name: &str) {
        match self.names.get(&guid) {
            None => {
                self.names.insert(guid, name.to_string());
            }
            Some(old) if old.eq_ignore_ascii_case(name) => {}
            Some(old) => {
                let old = old.clone();
                self.conflicts.push((guid, old, name.to_string()));
            }
        }
    }

    /// Take over everything `other` names (its conflicts too), the names of `self` standing where both name a GUID.
    pub fn extend(&mut self, other: GuidNames) {
        for (guid, name) in other.names {
            self.insert(guid, &name);
        }
        self.conflicts.extend(other.conflicts);
    }

    /// The name of the document with file GUID `guid`.
    pub fn get(&self, guid: &[u8; 16]) -> Option<&str> {
        self.names.get(guid).map(String::as_str)
    }

    /// The number of documents named.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether no document is named.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Every (file GUID, name), in GUID order.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8; 16], &str)> + '_ {
        self.names.iter().map(|(g, n)| (g, n.as_str()))
    }

    /// The GUIDs that were given two different names: (GUID, kept name, other name).
    pub fn conflicts(&self) -> &[([u8; 16], String, String)] {
        &self.conflicts
    }

    /// Read the file GUID of each EBX document of `bundle` (its payload is read and decoded for the GUID, which the EBX
    /// stores in the first bytes of its fixup chunk) and record it under the document's name. A document that two
    /// bundles share (the same file in the same archive) is read once per call; pass `done` to skip it across calls.
    pub fn add_bundle(
        &mut self,
        src: &Source<'_>,
        bundle: &Bundle,
        done: &mut BTreeSet<(u64, u32, u32)>,
    ) -> Added {
        let mut added = Added::default();
        for e in bundle.entries().filter(|e| e.kind == Kind::Ebx) {
            if !done.insert((e.file.0.encode(), e.file.1, e.file.2)) {
                continue;
            }
            let size = usize::try_from(e.size).unwrap_or(usize::MAX);
            match src
                .payload(e.file, size)
                .and_then(|b| crate::ebx::read_file_guid(&b))
            {
                Ok(guid) => {
                    self.insert(guid, &e.name);
                    added.documents = added.documents.saturating_add(1);
                }
                Err(_) => added.unreadable = added.unreadable.saturating_add(1),
            }
        }
        added
    }
}
