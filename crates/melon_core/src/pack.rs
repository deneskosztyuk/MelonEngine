//! `game.pak`: every asset of a packaged game in one file.
//!
//! Layout (little endian, blobs 8-byte aligned, no compression):
//!
//! ```text
//! header  32 bytes  magic "MELONPAK" | version u32 | count u32 | index_offset u64 | index_len u64
//! blobs   each file's bytes, padded with zeros to a multiple of 8
//! index   count times: path_len u32 | path (UTF-8, project-relative, "/") |
//!                      has_guid u8 | guid 16 bytes (zeros if none) | offset u64 | size u64
//! ```

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};

use crate::assets::{AssetGuid, AssetMeta, asset_dir_files, meta_path, project_relative};

pub const MAGIC: &[u8; 8] = b"MELONPAK";
pub const PACK_VERSION: u32 = 1;
const HEADER_LEN: u64 = 32;

/// One file to pack: its project-relative path, GUID and where it is on disk.
#[derive(Clone, Debug)]
pub struct PackFile {
    pub path: String,
    pub guid: Option<AssetGuid>,
    pub source: PathBuf,
}

#[derive(Clone, Copy)]
struct Entry {
    guid: Option<AssetGuid>,
    offset: u64,
    size: u64,
}

/// An open pack: the index in memory, blobs read on demand with seeks.
pub struct Pack {
    file: Mutex<File>,
    index: HashMap<String, Entry>,
}

/// The pack a running packaged game reads its `config/*.ron` from (the
/// settings loaders take disk paths): the project root and the pack.
static INSTALLED: std::sync::RwLock<Option<(PathBuf, std::sync::Arc<Pack>)>> =
    std::sync::RwLock::new(None);

/// Make `pack` answer reads of files under `root` (see `installed_read`).
pub fn install(root: &Path, pack: std::sync::Arc<Pack>) {
    *INSTALLED.write().unwrap_or_else(|p| p.into_inner()) = Some((root.to_path_buf(), pack));
}

/// `path` (under the installed root) from the installed pack. `None` when no
/// pack is installed or `path` is elsewhere; a packed game never falls back
/// to disk for files inside its root.
pub fn installed_read(path: &Path) -> Option<std::io::Result<Vec<u8>>> {
    let guard = INSTALLED.read().unwrap_or_else(|p| p.into_inner());
    let (root, pack) = guard.as_ref()?;
    let rel = project_relative(root, path)?;
    Some(
        pack.read(&rel)
            .unwrap_or_else(|| Err(std::io::ErrorKind::NotFound.into())),
    )
}

/// `*` matches within a path segment, `**` across segments, `?` one character.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => {
                let rest = &p[2..];
                (0..=s.len()).any(|i| go(rest, &s[i..]))
            }
            Some(b'*') => {
                let rest = &p[1..];
                (0..=s.len()).any(|i| go(rest, &s[i..]) && !s[..i].contains(&b'/'))
            }
            Some(b'?') => s.first().is_some_and(|c| *c != b'/') && go(&p[1..], &s[1..]),
            Some(c) => s.first() == Some(c) && go(&p[1..], &s[1..]),
        }
    }
    go(pattern.as_bytes(), path.as_bytes())
}

/// Everything a packaged game ships from `root`: files under `assets/` (not
/// `.meta` sidecars, whose GUIDs go into the index, and nothing that starts
/// with a dot) and `config/*.ron`, minus paths matching an exclude glob.
pub fn project_files(root: &Path, excludes: &[String]) -> Vec<PackFile> {
    let mut files = Vec::new();
    let skip = |rel: &str| excludes.iter().any(|g| glob_match(g, rel));
    for file in asset_dir_files(root) {
        let Some(rel) = project_relative(root, &file) else {
            continue;
        };
        if rel.ends_with(".meta") || skip(&rel) {
            continue;
        }
        let guid = AssetMeta::read(&meta_path(&file)).ok().map(|m| m.guid);
        files.push(PackFile {
            path: rel,
            guid,
            source: file,
        });
    }
    if let Ok(read) = std::fs::read_dir(root.join("config")) {
        let mut configs: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "ron"))
            .collect();
        configs.sort();
        for file in configs {
            let Some(rel) = project_relative(root, &file) else {
                continue;
            };
            if !skip(&rel) {
                files.push(PackFile {
                    path: rel,
                    guid: None,
                    source: file,
                });
            }
        }
    }
    files
}

/// Write `files` into the pack at `out`.
pub fn write_pack(out: &Path, files: &[PackFile]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    if let Some(f) = files.iter().find(|f| !seen.insert(f.path.as_str())) {
        bail!("two files are packed as {}", f.path);
    }
    let mut file = File::create(out).with_context(|| format!("cannot create {}", out.display()))?;
    file.write_all(&[0; HEADER_LEN as usize])?;
    let mut entries = Vec::with_capacity(files.len());
    let mut offset = HEADER_LEN;
    for f in files {
        let data = std::fs::read(&f.source)
            .with_context(|| format!("cannot read {}", f.source.display()))?;
        file.write_all(&data)?;
        let pad = (8 - data.len() % 8) % 8;
        file.write_all(&[0; 8][..pad])?;
        entries.push(Entry {
            guid: f.guid,
            offset,
            size: data.len() as u64,
        });
        offset += (data.len() + pad) as u64;
    }
    let mut index = Vec::new();
    for (f, e) in files.iter().zip(&entries) {
        index.extend((f.path.len() as u32).to_le_bytes());
        index.extend(f.path.as_bytes());
        index.push(e.guid.is_some() as u8);
        index.extend(e.guid.map_or([0; 16], |g| g.to_bytes()));
        index.extend(e.offset.to_le_bytes());
        index.extend(e.size.to_le_bytes());
    }
    file.write_all(&index)?;
    file.seek(SeekFrom::Start(0))?;
    let mut header = Vec::with_capacity(HEADER_LEN as usize);
    header.extend(MAGIC);
    header.extend(PACK_VERSION.to_le_bytes());
    header.extend((files.len() as u32).to_le_bytes());
    header.extend(offset.to_le_bytes());
    header.extend((index.len() as u64).to_le_bytes());
    file.write_all(&header)?;
    Ok(())
}

/// Pack `project_files(root, excludes)` into `out`; returns the file count.
pub fn build_pack(root: &Path, excludes: &[String], out: &Path) -> Result<usize> {
    let files = project_files(root, excludes);
    write_pack(out, &files)?;
    Ok(files.len())
}

/// Parse `count` index entries; every blob must lie inside a file of `file_len` bytes.
fn parse_index(bytes: &[u8], count: u32, file_len: u64) -> Result<HashMap<String, Entry>> {
    let mut at = 0usize;
    let mut take = |n: usize| -> Result<&[u8]> {
        let s = at
            .checked_add(n)
            .and_then(|end| bytes.get(at..end))
            .context("pack index is cut off")?;
        at += n;
        Ok(s)
    };
    let mut index = HashMap::new();
    for _ in 0..count {
        let len = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
        let name = String::from_utf8(take(len)?.to_vec()).context("pack path")?;
        let has_guid = take(1)?[0] != 0;
        let guid = AssetGuid::from_bytes(take(16)?.try_into().unwrap());
        let offset = u64::from_le_bytes(take(8)?.try_into().unwrap());
        let size = u64::from_le_bytes(take(8)?.try_into().unwrap());
        if offset.checked_add(size).is_none_or(|end| end > file_len) {
            bail!("pack entry {name} lies outside the file");
        }
        let guid = has_guid.then_some(guid);
        index.insert(name, Entry { guid, offset, size });
    }
    Ok(index)
}

impl Pack {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file =
            File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let mut header = [0u8; HEADER_LEN as usize];
        file.read_exact(&mut header).context("pack header")?;
        if &header[..8] != MAGIC {
            bail!("{} is not a MelonEngine pack", path.display());
        }
        let u32_at = |i: usize| u32::from_le_bytes(header[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(header[i..i + 8].try_into().unwrap());
        if u32_at(8) != PACK_VERSION {
            bail!("unsupported pack version {}", u32_at(8));
        }
        let (count, index_offset, index_len) = (u32_at(12), u64_at(16), u64_at(24));
        let file_len = file.metadata()?.len();
        if index_offset < HEADER_LEN
            || index_offset
                .checked_add(index_len)
                .is_none_or(|e| e > file_len)
        {
            bail!(
                "{} is cut off or corrupt (index outside the file)",
                path.display()
            );
        }
        file.seek(SeekFrom::Start(index_offset))?;
        let mut bytes = vec![0u8; index_len as usize];
        file.read_exact(&mut bytes).context("pack index")?;
        let index = parse_index(&bytes, count, file_len)?;
        Ok(Self {
            file: Mutex::new(file),
            index,
        })
    }

    pub fn contains(&self, path: &str) -> bool {
        self.index.contains_key(path)
    }

    /// The file's bytes; `None` if the pack has no such path.
    pub fn read(&self, path: &str) -> Option<std::io::Result<Vec<u8>>> {
        let e = self.index.get(path)?;
        let mut file = self.file.lock().unwrap_or_else(|p| p.into_inner());
        let mut buf = vec![0u8; e.size as usize];
        Some(
            file.seek(SeekFrom::Start(e.offset))
                .and_then(|_| file.read_exact(&mut buf))
                .map(|_| buf),
        )
    }

    /// Every packed path with its GUID.
    pub fn entries(&self) -> impl Iterator<Item = (&str, Option<AssetGuid>)> {
        self.index.iter().map(|(p, e)| (p.as_str(), e.guid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_pack_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets/models")).unwrap();
        std::fs::create_dir_all(dir.join("assets/scenes")).unwrap();
        std::fs::create_dir_all(dir.join("assets/.thumbs")).unwrap();
        std::fs::create_dir_all(dir.join("config/quality_profiles")).unwrap();
        dir
    }

    /// A file with a `.meta` sidecar; returns its GUID.
    fn asset(root: &Path, rel: &str, bytes: &[u8]) -> AssetGuid {
        std::fs::write(root.join(rel), bytes).unwrap();
        let meta = AssetMeta {
            version: crate::assets::META_VERSION,
            guid: AssetGuid::new(),
        };
        std::fs::write(meta_path(&root.join(rel)), meta.to_ron()).unwrap();
        meta.guid
    }

    #[test]
    fn glob_matches_segments_and_trees() {
        assert!(glob_match("assets/*.md", "assets/a.md"));
        assert!(!glob_match("assets/*.md", "assets/x/a.md"));
        assert!(glob_match("assets/**/*.md", "assets/x/y/a.md"));
        assert!(glob_match("**/*.md", "assets/x/a.md"));
        assert!(glob_match("assets/raw/**", "assets/raw/a/b.png"));
        assert!(glob_match("a?c", "abc") && !glob_match("a?c", "a/c"));
        assert!(!glob_match("assets/raw/**", "assets/rawer/a.png"));
    }

    #[test]
    fn pack_round_trips_every_file_byte_identical_with_guids() {
        let root = temp_project("roundtrip");
        let g1 = asset(&root, "assets/models/a.glb", b"glb-bytes");
        let g2 = asset(
            &root,
            "assets/scenes/main.scene",
            &[0, 1, 2, 3, 4, 5, 6, 7, 8],
        );
        asset(&root, "assets/empty.bin", b"");
        std::fs::write(root.join("config/input.ron"), "(map: 1)").unwrap();
        let out = root.join("game.pak");
        let n = build_pack(&root, &[], &out).unwrap();
        assert_eq!(n, 4);

        let pack = Pack::open(&out).unwrap();
        assert_eq!(
            pack.read("assets/models/a.glb").unwrap().unwrap(),
            b"glb-bytes"
        );
        assert_eq!(
            pack.read("assets/scenes/main.scene").unwrap().unwrap(),
            [0, 1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert!(pack.read("assets/empty.bin").unwrap().unwrap().is_empty());
        assert_eq!(pack.read("config/input.ron").unwrap().unwrap(), b"(map: 1)");
        assert!(pack.read("assets/missing").is_none());
        let guids: HashMap<&str, Option<AssetGuid>> = pack.entries().collect();
        assert_eq!(guids["assets/models/a.glb"], Some(g1));
        assert_eq!(guids["assets/scenes/main.scene"], Some(g2));
        assert_eq!(guids["config/input.ron"], None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn blobs_are_eight_byte_aligned() {
        let root = temp_project("align");
        asset(&root, "assets/a.bin", b"abc");
        asset(&root, "assets/b.bin", b"defgh");
        let out = root.join("game.pak");
        build_pack(&root, &[], &out).unwrap();
        let pack = Pack::open(&out).unwrap();
        assert!(pack.index.values().all(|e| e.offset % 8 == 0));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn excluded_meta_hidden_and_profile_files_are_absent() {
        let root = temp_project("exclude");
        asset(&root, "assets/models/a.glb", b"a");
        asset(&root, "assets/notes.md", b"notes");
        std::fs::write(root.join("assets/.thumbs/t.png"), b"t").unwrap();
        std::fs::write(root.join("config/quality.ron"), "()").unwrap();
        std::fs::write(root.join("config/quality_profiles/p.ron"), "()").unwrap();
        let files = project_files(&root, &["**/*.md".to_string()]);
        let mut paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, ["assets/models/a.glb", "config/quality.ron"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_rejects_a_file_that_is_not_a_pack() {
        let root = temp_project("bad");
        std::fs::write(root.join("x.pak"), vec![7u8; 64]).unwrap();
        assert!(Pack::open(&root.join("x.pak")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A pack with the given (path, bytes) files, written from a temp project.
    fn pack_of(name: &str, files: &[(&str, &[u8])]) -> (PathBuf, PathBuf) {
        let root = temp_project(name);
        let mut list = Vec::new();
        for (i, (path, bytes)) in files.iter().enumerate() {
            let source = root.join(format!("src{i}.bin"));
            std::fs::write(&source, bytes).unwrap();
            list.push(PackFile {
                path: path.to_string(),
                guid: None,
                source,
            });
        }
        let out = root.join("game.pak");
        write_pack(&out, &list).unwrap();
        (root, out)
    }

    #[test]
    fn an_empty_pack_opens_and_has_nothing() {
        let (root, out) = pack_of("empty", &[]);
        let pack = Pack::open(&out).unwrap();
        assert_eq!(pack.entries().count(), 0);
        assert!(pack.read("assets/a").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn spaces_and_non_ascii_paths_round_trip_from_disk() {
        let root = temp_project("unicode");
        std::fs::create_dir_all(root.join("assets/ünï cödé")).unwrap();
        let g = asset(&root, "assets/ünï cödé/モデル a b.bin", b"x");
        let files = project_files(&root, &[]);
        assert_eq!(files[0].path, "assets/ünï cödé/モデル a b.bin");
        let out = root.join("game.pak");
        write_pack(&out, &files).unwrap();
        let pack = Pack::open(&out).unwrap();
        assert_eq!(
            pack.read("assets/ünï cödé/モデル a b.bin")
                .unwrap()
                .unwrap(),
            b"x"
        );
        assert_eq!(pack.entries().next().unwrap().1, Some(g));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn duplicate_paths_are_refused_when_writing() {
        let root = temp_project("dups");
        let source = root.join("a.bin");
        std::fs::write(&source, b"a").unwrap();
        let f = PackFile {
            path: "assets/a.bin".into(),
            guid: None,
            source,
        };
        assert!(write_pack(&root.join("game.pak"), &[f.clone(), f]).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_truncated_pack_is_an_error_not_a_panic() {
        let (root, out) = pack_of(
            "trunc",
            &[("assets/a.bin", b"hello"), ("assets/b.bin", b"world!")],
        );
        let full = std::fs::read(&out).unwrap();
        for len in 0..full.len() {
            std::fs::write(&out, &full[..len]).unwrap();
            assert!(
                Pack::open(&out).is_err(),
                "opened a pack cut to {len} bytes"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_corrupt_header_or_index_is_an_error_not_a_panic() {
        let (root, out) = pack_of("corrupt", &[("assets/a.bin", b"hello")]);
        let good = std::fs::read(&out).unwrap();
        let patch = |at: usize, bytes: &[u8]| {
            let mut v = good.clone();
            v[at..at + bytes.len()].copy_from_slice(bytes);
            std::fs::write(&out, v).unwrap();
        };
        patch(24, &u64::MAX.to_le_bytes()); // index length
        assert!(Pack::open(&out).is_err(), "huge index length");
        patch(16, &u64::MAX.to_le_bytes()); // index offset
        assert!(Pack::open(&out).is_err(), "index offset past the end");
        patch(12, &u32::MAX.to_le_bytes()); // entry count
        assert!(Pack::open(&out).is_err(), "count larger than the index");
        patch(8, &99u32.to_le_bytes());
        assert!(Pack::open(&out).is_err(), "unknown version");
        let at = good.len() - 16; // the entry's offset field
        patch(at, &(u64::MAX - 2).to_le_bytes());
        assert!(Pack::open(&out).is_err(), "blob offset past the end");
        let at = good.len() - 8; // its size field
        patch(at, &(1u64 << 62).to_le_bytes());
        assert!(Pack::open(&out).is_err(), "huge blob size");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn offsets_past_4_gib_do_not_truncate() {
        let offset = (5u64 << 32) + 8;
        let mut index = Vec::new();
        index.extend(5u32.to_le_bytes());
        index.extend(b"a.bin");
        index.push(0);
        index.extend([0u8; 16]);
        index.extend(offset.to_le_bytes());
        index.extend(3u64.to_le_bytes());
        let parsed = parse_index(&index, 1, offset + 8).unwrap();
        assert_eq!((parsed["a.bin"].offset, parsed["a.bin"].size), (offset, 3));
        assert!(
            parse_index(&index, 1, offset + 2).is_err(),
            "blob ends past the file"
        );
    }
}
