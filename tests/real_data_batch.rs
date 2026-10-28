//! Readings of the skate. install and of the mods installed beside it, checked over every file they reach
//! (`installs.skate/Data` with `tools.oodle9`; `installs.skate/Mods` for the mods, which a machine without them skips):
//!
//! - every bundle manifest's `chunkMeta` reads to typed rows, one per chunk, and is written back byte for byte;
//! - `layout.toc` reads, is written back byte for byte, names the TOCs the folder holds, and gives the install chunks
//!   the directories the archives are found by;
//! - a Patch TOC that asks for its base game's bundles and chunks is merged with the base TOC and the merged TOC
//!   stands alone: its perfect hashes resolve, it writes and reads back.

#![allow(clippy::unwrap_used)]

mod inputs;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use frostbite_formats::bundle::{self, ChunkMetaBody};
use frostbite_formats::folder::{Folder, Source};
use frostbite_formats::layout;
use frostbite_formats::toc;
use inputs::{Game, Tool, install_file, tool};
use primase_foundations::compress::oodle::{Oodle, Version};

type R = Result<(), Box<dyn std::error::Error>>;

/// The folders under `mods` that hold a `layout.toc` and a `Win32` folder (a mod's data folder), sorted.
fn mod_roots(mods: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if dir.join("layout.toc").is_file() && dir.join("Win32").is_dir() {
            out.push(dir.to_path_buf());
            return;
        }
        if depth >= 3 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut dirs: Vec<_> = rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for d in dirs {
            walk(&d, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(mods, 0, &mut out);
    out
}

struct Inputs {
    stock: Folder,
    mods: Vec<Folder>,
    oodle: Oodle,
}

fn inputs() -> Result<Option<Inputs>, Box<dyn std::error::Error>> {
    let (Some(data), Some(dll)) = (install_file(Game::Skate, "Data"), tool(Tool::Oodle9)) else {
        return Ok(None);
    };
    let stock = Folder::open(data)?;
    // SAFETY: the configuration names the game's own Oodle DLL as tools.oodle9.
    #[allow(unsafe_code)]
    let oodle = unsafe { Oodle::load(dll, Version::V9) }?;
    let mods = match install_file(Game::Skate, "Mods") {
        Some(dir) => mod_roots(&dir)
            .into_iter()
            .map(Folder::open)
            .collect::<Result<Vec<_>, _>>()?,
        None => Vec::new(),
    };
    Ok(Some(Inputs { stock, mods, oodle }))
}

#[derive(Default)]
struct ChunkMetaTally {
    bundles: u64,
    rows: u64,
    empty: u64,
    first_mip: u64,
    container: u64,
    metrics: u64,
    metric_names: BTreeSet<String>,
    no_metadata: u64,
    problems: Vec<String>,
}

fn chunk_meta_of(folder: &Folder, base: &Folder, oodle: &Oodle, t: &mut ChunkMetaTally) {
    let src = Source {
        folder,
        base: Some(base),
        oodle: Some(oodle),
    };
    for toc in folder.tocs.clone() {
        let Ok(listing) = src.listing(&toc) else {
            t.problems.push(format!("{toc}: does not list"));
            continue;
        };
        for b in &listing.bundles {
            t.bundles += 1;
            let m = &b.manifest;
            let rows = match m.chunk_meta() {
                Ok(r) => r,
                Err(e) => {
                    t.problems.push(format!("{}: {e}", b.name));
                    continue;
                }
            };
            if !m.meta.is_empty() && rows.len() != m.chunks.len() {
                t.problems.push(format!(
                    "{}: {} rows for {} chunks",
                    b.name,
                    rows.len(),
                    m.chunks.len()
                ));
            }
            // a bundle without chunks stores no metadata in the stock data (the empty list in Studio's)
            if m.meta.is_empty() && rows.is_empty() {
                t.no_metadata += 1;
                continue;
            }
            match bundle::write_chunk_meta(&rows) {
                Ok(w) if w == m.meta => {}
                Ok(w) => {
                    let at = w
                        .iter()
                        .zip(&m.meta)
                        .position(|(a, b)| a != b)
                        .unwrap_or(w.len().min(m.meta.len()));
                    t.problems.push(format!(
                        "{}: the rows write {} bytes, {} are stored, first difference at {at} ({:02x?} against {:02x?})",
                        b.name,
                        w.len(),
                        m.meta.len(),
                        w.get(at.saturating_sub(4)..(at + 8).min(w.len())),
                        m.meta.get(at.saturating_sub(4)..(at + 8).min(m.meta.len()))
                    ));
                }
                Err(e) => t.problems.push(format!("{}: {e}", b.name)),
            }
            for r in rows {
                t.rows += 1;
                match r.meta {
                    ChunkMetaBody::Empty => t.empty += 1,
                    ChunkMetaBody::FirstMip(_) => t.first_mip += 1,
                    ChunkMetaBody::Container { metrics, .. } => {
                        t.container += 1;
                        t.metrics += metrics.len() as u64;
                        t.metric_names.extend(metrics.into_iter().map(|m| m.name));
                    }
                }
            }
        }
    }
}

#[test]
fn chunk_meta_rows_type_and_write_back_on_every_manifest() -> R {
    let Some(i) = inputs()? else { return Ok(()) };
    let mut stock = ChunkMetaTally::default();
    chunk_meta_of(&i.stock, &i.stock, &i.oodle, &mut stock);
    println!(
        "stock: {} bundles, {} rows ({} empty, {} firstMip, {} container with {} metrics); problems {:?}",
        stock.bundles,
        stock.rows,
        stock.empty,
        stock.first_mip,
        stock.container,
        stock.metrics,
        stock.problems
    );
    assert!(stock.problems.is_empty(), "{:?}", stock.problems);
    // the census of build 20260929: 6,056 bundles, 239,810 rows, 175,203 empty, 58,570 firstMip, 6,037 containers
    assert_eq!(stock.bundles, 6_056);
    assert_eq!(stock.no_metadata, 17, "the bundles without chunks");
    assert_eq!(stock.rows, 239_810);
    assert_eq!(stock.empty, 175_203);
    assert_eq!(stock.first_mip, 58_570);
    assert_eq!(stock.container, 6_037);
    assert_eq!(stock.metrics, 6 * 6_037);
    println!("metric names: {:?}", stock.metric_names);
    assert_eq!(stock.metric_names.len(), 6, "{:?}", stock.metric_names);
    let mut mods = ChunkMetaTally::default();
    for folder in &i.mods {
        chunk_meta_of(folder, &i.stock, &i.oodle, &mut mods);
    }
    println!(
        "mods: {} folders, {} bundles, {} rows ({} empty, {} firstMip, {} container); problems {:?}",
        i.mods.len(),
        mods.bundles,
        mods.rows,
        mods.empty,
        mods.first_mip,
        mods.container,
        mods.problems.iter().take(3).collect::<Vec<_>>()
    );
    println!("{} mod bundles without metadata", mods.no_metadata);
    assert!(mods.problems.is_empty(), "{:?}", mods.problems);
    Ok(())
}

/// `Win32/globals` names `Win32/globals.toc`; TOC paths compare lowercase.
fn toc_names(folder: &Folder) -> BTreeSet<String> {
    folder
        .tocs
        .iter()
        .map(|t| t.strip_suffix(".toc").unwrap_or(t).to_lowercase())
        .collect()
}

#[test]
fn layouts_read_write_back_and_name_the_tocs_of_their_folder() -> R {
    let Some(i) = inputs()? else { return Ok(()) };
    let check = |folder: &Folder| -> Result<(usize, usize, usize), Box<dyn std::error::Error>> {
        let path = folder.root.join("layout.toc");
        let bytes = std::fs::read(&path)?;
        let l = layout::read(&bytes)?;
        assert_eq!(l.write()?, bytes, "{}: layout.toc written back", path.display());
        assert_eq!(l, folder.layout);
        let listed: BTreeSet<String> = l.super_bundles.iter().map(|n| n.to_lowercase()).collect();
        let on_disk = toc_names(folder);
        // every TOC in the folder is a listed superbundle
        let unlisted: Vec<_> = on_disk.difference(&listed).collect();
        assert!(
            unlisted.is_empty(),
            "{}: TOCs the layout does not list: {unlisted:?}",
            path.display()
        );
        assert_eq!(l.fs, ["initfs_Win32"], "{}", path.display());
        assert_eq!(l.head, Some(625_200));
        assert_eq!(l.bundle_format, Some(2_651_454_669));
        // `name` and `installBundle` give one directory, and the install chunks have persistent indices
        for c in &l.install_chunks {
            assert!(c.persistent_index.is_some(), "{}: {c:?}", path.display());
            if let Some(b) = &c.install_bundle {
                assert_eq!(b, &format!("Win32/{}", c.name), "{}", path.display());
            }
        }
        Ok((l.super_bundles.len(), on_disk.len(), l.install_chunks.len()))
    };
    let (listed, on_disk, chunks) = check(&i.stock)?;
    println!("stock layout: {listed} superbundles listed, {on_disk} TOCs on disk, {chunks} install chunks");
    assert_eq!((listed, on_disk, chunks), (24, 23, 16));
    assert_eq!(i.stock.layout.install_groups.len(), 2);
    let (mut names, mut tocs) = (0, 0);
    for folder in &i.mods {
        let (n, t, _) = check(folder)?;
        names += n;
        tocs += t;
    }
    println!(
        "{} mod layouts: {names} superbundles listed, {tocs} TOCs on disk",
        i.mods.len()
    );
    Ok(())
}

#[derive(Default)]
struct MergeTally {
    tocs: u64,
    flagged: u64,
    flagged_with_base: u64,
    flagged_without_base: u64,
    patch_bundles: u64,
    bundles_replacing: u64,
    bundles_new: u64,
    patch_chunks: u64,
    chunk_references: u64,
    bundle_records: u64,
    chunks_overriding: u64,
    chunks_new: u64,
    chunks_removed: u64,
    problems: Vec<String>,
}

#[test]
fn patch_tocs_merge_with_their_base() -> R {
    let Some(i) = inputs()? else { return Ok(()) };
    let mut t = MergeTally::default();
    // the stock TOCs carry the Huffman flag only: nothing to merge, the TOC as it is
    for rel in i.stock.tocs.clone() {
        let src = Source {
            folder: &i.stock,
            base: None,
            oodle: Some(&i.oodle),
        };
        let merged = src.merged_toc(&rel)?;
        assert_eq!(merged, i.stock.toc(&rel)?, "{rel}");
        assert_eq!(merged.flags, toc::flags::HUFFMAN_NAMES, "{rel}");
    }
    for folder in &i.mods {
        let src = Source {
            folder,
            base: Some(&i.stock),
            oodle: Some(&i.oodle),
        };
        for rel in folder.tocs.clone() {
            t.tocs += 1;
            let patch = folder.toc(&rel)?;
            t.chunk_references += patch.chunks.len() as u64;
            t.bundle_records += patch.bundles.len() as u64;
            let merged = src.merged_toc(&rel)?;
            toc::verify(&merged).map_err(|e| format!("{}: {rel}: {e}", folder.root.display()))?;
            // it writes and reads back to the same bundles and chunks
            let again = toc::read(&toc::write(&merged.bundles, &merged.chunks, merged.flags)?)?;
            assert_eq!(
                (&again.bundles, &again.chunks),
                (&merged.bundles, &merged.chunks),
                "{rel}"
            );
            if !patch.has_base_bundles() && !patch.has_base_chunks() {
                assert_eq!(merged, patch, "{rel}: an unflagged TOC is its own view");
                continue;
            }
            assert!(
                patch.has_base_bundles() && patch.has_base_chunks(),
                "{rel}: flags {}",
                patch.flags
            );
            t.flagged += 1;
            if i.stock.tocs.contains(&rel) {
                t.flagged_with_base += 1;
                let base = i.stock.toc(&rel)?;
                let patch_names: BTreeSet<String> =
                    patch.bundles.iter().map(|(n, _)| n.to_lowercase()).collect();
                let base_names: BTreeSet<String> =
                    base.bundles.iter().map(|(n, _)| n.to_lowercase()).collect();
                t.patch_bundles += patch_names.len() as u64;
                t.bundles_replacing += patch_names.intersection(&base_names).count() as u64;
                t.bundles_new += patch_names.difference(&base_names).count() as u64;
                let patch_guids: BTreeSet<[u8; 16]> = patch.chunks.iter().map(|c| c.guid).collect();
                let base_guids: BTreeSet<[u8; 16]> = base.chunks.iter().map(|c| c.guid).collect();
                t.patch_chunks += patch_guids.len() as u64;
                t.chunks_overriding += patch_guids.intersection(&base_guids).count() as u64;
                t.chunks_new += patch_guids.difference(&base_guids).count() as u64;
                t.chunks_removed += patch.chunks.iter().filter(|c| c.file.is_none()).count() as u64;
                // the merged view: every base bundle and chunk the patch does not replace, then the patch's
                let expect_bundles = base_names.union(&patch_names).count();
                let expect_chunks = base_guids.union(&patch_guids).count();
                if merged.bundles.len() != expect_bundles || merged.chunks.len() != expect_chunks {
                    t.problems.push(format!(
                        "{rel}: merged {} bundles / {} chunks, expected {expect_bundles} / {expect_chunks}",
                        merged.bundles.len(),
                        merged.chunks.len()
                    ));
                }
                // a patch bundle's region is the one the merged view has
                for (n, region) in &patch.bundles {
                    if merged
                        .bundles
                        .iter()
                        .find(|(m, _)| m.eq_ignore_ascii_case(n))
                        .map(|(_, r)| r)
                        != Some(region)
                    {
                        t.problems.push(format!("{rel}: bundle {n} is not the patch's"));
                    }
                }
                let by_guid: BTreeMap<_, _> = merged.chunks.iter().map(|c| (c.guid, c.file)).collect();
                for c in patch.chunks.iter().filter(|c| c.file.is_some()) {
                    if by_guid.get(&c.guid) != Some(&c.file) {
                        t.problems
                            .push(format!("{rel}: chunk {:02x?} is not the patch's", c.guid));
                    }
                }
            } else {
                t.flagged_without_base += 1;
                assert_eq!(merged.bundles.len(), patch.bundles.len(), "{rel}");
                assert_eq!(merged.chunks.len(), patch.chunks.len(), "{rel}");
                assert!(patch.bundles.iter().all(|b| merged.bundles.contains(b)), "{rel}");
            }
        }
    }
    println!(
        "mods: {} TOCs with {} bundle records and {} chunk references, {} flagged ({} with a base TOC, {} without); patch bundles {} ({} replace a base bundle, {} new); patch chunks {} ({} override, {} new, {} removed); problems {:?}",
        t.tocs,
        t.bundle_records,
        t.chunk_references,
        t.flagged,
        t.flagged_with_base,
        t.flagged_without_base,
        t.patch_bundles,
        t.bundles_replacing,
        t.bundles_new,
        t.patch_chunks,
        t.chunks_overriding,
        t.chunks_new,
        t.chunks_removed,
        t.problems
    );
    assert!(t.problems.is_empty(), "{:?}", t.problems);
    if !i.mods.is_empty() {
        // the census of the installed mods (read-only scripts over the raw bytes): 245 TOCs, 199 flagged, 169 with a base
        assert!(t.flagged > 0);
        assert_eq!(t.bundles_new, 0);
        assert_eq!(t.chunks_removed, 0);
    }
    Ok(())
}
