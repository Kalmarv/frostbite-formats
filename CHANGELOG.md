# Changelog

## 0.1.0 (unreleased)

- First release: readers and writers for the TOC, bundle, CAS, native DB, EBX, `.fbmod`, shader, MeshSet, physics, skeleton and texture formats of skate.
- `ebx::remap` gives a document's identities (file, instance, import and GUID values, and the bytes it carries) new GUIDs, so a clone and its original can both be loaded; `guid_text` writes a GUID in its text form.
