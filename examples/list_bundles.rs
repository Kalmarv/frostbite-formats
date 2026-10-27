//! List the bundles of one superbundle TOC of the game's `Data` folder, with the number of EBX documents, resources
//! and chunks each holds:
//!
//! ```text
//! cargo run --example list_bundles -- <game>/Data Win32/globals.toc <game>/oo2core_9_win64.dll
//! ```
//!
//! The first argument is the game's `Data` folder (it holds `layout.toc` and `Win32`), the second a TOC relative to it,
//! such as `Win32/globals.toc`. The third is the game's own Oodle library, which the bundle manifests need on Windows
//! (elsewhere a portable decoder is used when it is left out). Nothing is written.
#![allow(
    unsafe_code,
    reason = "loads the game's own Oodle library, named by the caller"
)]

use std::path::PathBuf;

use frostbite_formats::bundle::Kind;
use frostbite_formats::folder::{Folder, Source};
use primase_foundations::compress::oodle::{Oodle, Version};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let (Some(data), Some(toc)) = (args.next(), args.next()) else {
        eprintln!("usage: list_bundles <data folder> <toc, relative to it> [oodle library]");
        std::process::exit(2);
    };
    let oodle = match args.next() {
        // SAFETY: the library is the one the caller names and trusts: loading it runs its initialisation code.
        Some(dll) => Some(unsafe { Oodle::load(PathBuf::from(dll), Version::V9) }?),
        #[cfg(not(windows))]
        None => Some(Oodle::portable()),
        #[cfg(windows)]
        None => None,
    };

    let folder = Folder::open(data)?;
    let source = Source {
        folder: &folder,
        base: Some(&folder),
        oodle: oodle.as_ref(),
    };
    let toc = toc.to_string_lossy();
    let listing = source.listing(&toc)?;
    for bundle in &listing.bundles {
        let count = |kind| bundle.entries().filter(|e| e.kind == kind).count();
        println!(
            "{}: {} EBX, {} resources, {} chunks",
            bundle.name,
            count(Kind::Ebx),
            count(Kind::Res),
            count(Kind::Chunk)
        );
    }
    for (name, why) in &listing.unread {
        println!("{name}: not read: {why}");
    }
    Ok(())
}
