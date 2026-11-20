//! Exports `MELON_CORE_SOURCE_HASH`, a digest of every file under `src/`, so
//! `abi_fingerprint` changes whenever engine source changes.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::{fs, io};

fn collect(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=src");
    let mut files = Vec::new();
    collect(Path::new("src"), &mut files)?;
    files.sort();
    let mut hasher = DefaultHasher::new();
    for file in &files {
        file.hash(&mut hasher);
        fs::read(file)?.hash(&mut hasher);
    }
    println!(
        "cargo:rustc-env=MELON_CORE_SOURCE_HASH={:016x}",
        hasher.finish()
    );
    Ok(())
}
