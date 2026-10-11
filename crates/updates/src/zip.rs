//! Unpacking a package zip without trusting it (docs/UPDATES.md,
//! "Downloading and staging", step 4).
//!
//! The package has already matched the signed manifest's size and SHA-256
//! when this runs, so these checks are a second line: a mistake in a
//! packaging script, or a signing key used carelessly, must still not be
//! able to write outside the staging folder.
//!
//! A deliberately small reader: the central directory is the only index
//! (local headers must agree with it), stored and deflated entries only,
//! no encryption, no zip64, no multi-part archives. Everything is checked
//! before the first byte is written.

use crate::{Cancel, Reason, UpdateError};
use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Limits on what a package may unpack to.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_entries: usize,
    /// Sum of all entries' unpacked sizes.
    pub max_total: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_entries: 200_000,
            max_total: 4 * 1024 * 1024 * 1024,
        }
    }
}

const EOCD_SIG: u32 = 0x0605_4b50;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
const MAX_CENTRAL_DIRECTORY: u64 = 64 * 1024 * 1024;
const MAX_SYMLINK_TARGET: u64 = 1024;
const MAX_PATH: usize = 1024;
const MAX_COMPONENT: usize = 255;
const MAX_LINK_DEPTH: usize = 16;
const CHUNK: usize = 64 * 1024;

const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;
const S_IFLNK: u32 = 0o120_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug)]
struct Entry {
    /// Validated components.
    path: Vec<String>,
    kind: Kind,
    method: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    executable: bool,
    /// Where the entry's data starts.
    data_start: u64,
    local_offset: u64,
}

fn unsafe_archive(why: impl Into<String>) -> UpdateError {
    UpdateError::new(Reason::UnsafeArchive, why)
}

fn io_error(what: &str, e: std::io::Error) -> UpdateError {
    UpdateError::new(Reason::Staging, format!("{what}: {e}"))
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Unpacks `zip` into `dest`, which must not exist yet (it is created).
/// On an error, `dest` may hold part of the package; the caller removes
/// it.
pub fn extract(
    zip: &Path,
    dest: &Path,
    limits: Limits,
    cancel: &Cancel,
) -> Result<(), UpdateError> {
    let mut file = File::open(zip).map_err(|e| io_error("opening the package", e))?;
    let entries = read_index(&mut file, limits)?;
    let links = check_symlinks(&mut file, &entries)?;

    fs::create_dir(dest).map_err(|e| io_error("creating the unpack folder", e))?;
    for e in entries.iter().filter(|e| e.kind == Kind::Dir) {
        create_dirs(dest, &e.path)?;
    }
    for e in entries.iter().filter(|e| e.kind == Kind::File) {
        if cancel.is_canceled() {
            return Err(UpdateError::new(Reason::Canceled, "canceled"));
        }
        if let Some((_, parent)) = e.path.split_last() {
            create_dirs(dest, parent)?;
        }
        let target = join(dest, &e.path);
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|err| io_error("creating a file", err))?;
        copy_entry(&mut file, e, &mut out, cancel)?;
        out.flush().map_err(|err| io_error("writing a file", err))?;
        drop(out);
        set_mode(&target, if e.executable { 0o755 } else { 0o644 })?;
    }
    // Links last, so nothing is ever written through one.
    for (e, target) in entries
        .iter()
        .filter(|e| e.kind == Kind::Symlink)
        .zip(&links)
    {
        if let Some((_, parent)) = e.path.split_last() {
            create_dirs(dest, parent)?;
        }
        make_symlink(target, &join(dest, &e.path))?;
    }
    Ok(())
}

fn join(dest: &Path, parts: &[String]) -> PathBuf {
    let mut p = dest.to_path_buf();
    for c in parts {
        p.push(c);
    }
    p
}

fn create_dirs(dest: &Path, parts: &[String]) -> Result<(), UpdateError> {
    let mut p = dest.to_path_buf();
    for c in parts {
        p.push(c);
        match fs::create_dir(&p) {
            Ok(()) => set_mode(&p, 0o755)?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Ours from an earlier entry; never a link (links come last).
                let meta =
                    fs::symlink_metadata(&p).map_err(|e| io_error("checking a folder", e))?;
                if !meta.is_dir() {
                    return Err(unsafe_archive("a folder's path is taken by a file"));
                }
            }
            Err(e) => return Err(io_error("creating a folder", e)),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), UpdateError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| io_error("setting permissions", e))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), UpdateError> {
    Ok(())
}

#[cfg(unix)]
fn make_symlink(target: &str, at: &Path) -> Result<(), UpdateError> {
    std::os::unix::fs::symlink(target, at).map_err(|e| io_error("creating a link", e))
}

#[cfg(not(unix))]
fn make_symlink(_target: &str, _at: &Path) -> Result<(), UpdateError> {
    // check_symlinks refuses every link on these platforms.
    Err(unsafe_archive("symbolic links are not supported here"))
}

/// Reads and checks the central directory and every local header.
fn read_index(file: &mut File, limits: Limits) -> Result<Vec<Entry>, UpdateError> {
    let len = file
        .metadata()
        .map_err(|e| io_error("reading the package", e))?
        .len();
    if len < 22 {
        return Err(unsafe_archive("not a zip file"));
    }
    // The end record is the last 22 bytes plus a comment of up to 64 KiB.
    let tail_len = len.min(22 + 65_535);
    let mut tail = vec![0; tail_len as usize];
    read_at(file, len - tail_len, &mut tail)?;
    let eocd = (0..=tail.len() - 22)
        .rev()
        .find(|&i| {
            u32_at(&tail, i) == EOCD_SIG
                && i + 22 + usize::from(u16_at(&tail, i + 20)) == tail.len()
        })
        .ok_or_else(|| unsafe_archive("no zip end record"))?;
    let rec = &tail[eocd..eocd + 22];
    let eocd_pos = len - tail_len + eocd as u64;
    if eocd >= 20 && u32_at(&tail, eocd - 20) == ZIP64_LOCATOR_SIG {
        return Err(unsafe_archive("zip64 archives are not supported"));
    }
    let (disk, cd_disk) = (u16_at(rec, 4), u16_at(rec, 6));
    let (on_disk, total) = (u16_at(rec, 8), u16_at(rec, 10));
    let (cd_size, cd_offset) = (u64::from(u32_at(rec, 12)), u64::from(u32_at(rec, 16)));
    if disk != 0 || cd_disk != 0 || on_disk != total {
        return Err(unsafe_archive("multi-part archives are not supported"));
    }
    if total == 0xffff || cd_size == 0xffff_ffff || cd_offset == 0xffff_ffff {
        return Err(unsafe_archive("zip64 archives are not supported"));
    }
    if usize::from(total) > limits.max_entries {
        return Err(unsafe_archive("too many entries"));
    }
    if cd_offset
        .checked_add(cd_size)
        .is_none_or(|end| end > eocd_pos)
        || cd_size > MAX_CENTRAL_DIRECTORY
    {
        return Err(unsafe_archive("central directory out of bounds"));
    }
    let mut cd = vec![0; cd_size as usize];
    read_at(file, cd_offset, &mut cd)?;

    let mut entries = Vec::with_capacity(usize::from(total));
    let mut seen: HashMap<String, Kind> = HashMap::new();
    let mut total_size = 0u64;
    let mut at = 0usize;
    for _ in 0..total {
        if at + 46 > cd.len() || u32_at(&cd, at) != CENTRAL_SIG {
            return Err(unsafe_archive("malformed central directory"));
        }
        let h = &cd[at..at + 46];
        let made_by_host = h[5];
        let flags = u16_at(h, 8);
        let method = u16_at(h, 10);
        let crc = u32_at(h, 16);
        let compressed = u64::from(u32_at(h, 20));
        let size = u64::from(u32_at(h, 24));
        let (name_len, extra_len, comment_len) = (
            usize::from(u16_at(h, 28)),
            usize::from(u16_at(h, 30)),
            usize::from(u16_at(h, 32)),
        );
        let start_disk = u16_at(h, 34);
        let external = u32_at(h, 38);
        let local_offset = u64::from(u32_at(h, 42));
        let end = at + 46 + name_len + extra_len + comment_len;
        if end > cd.len() {
            return Err(unsafe_archive("malformed central directory"));
        }
        let name = cd[at + 46..at + 46 + name_len].to_vec();
        at = end;

        if flags & 0x0001 != 0 || flags & 0x0040 != 0 || flags & 0x2000 != 0 {
            return Err(unsafe_archive("encrypted entries are not supported"));
        }
        if method != 0 && method != 8 {
            return Err(unsafe_archive("unsupported compression method"));
        }
        if compressed == 0xffff_ffff
            || size == 0xffff_ffff
            || local_offset == 0xffff_ffff
            || start_disk != 0
        {
            return Err(unsafe_archive("zip64 archives are not supported"));
        }
        if method == 0 && compressed != size {
            return Err(unsafe_archive("a stored entry's sizes disagree"));
        }
        let text =
            std::str::from_utf8(&name).map_err(|_| unsafe_archive("file name is not text"))?;
        let unix_mode = match made_by_host {
            3 | 19 => external >> 16, // Unix, OS X
            _ => 0,
        };
        let is_dir_name = text.ends_with('/') || text.ends_with('\\');
        let kind = match unix_mode & S_IFMT {
            0 | S_IFREG if is_dir_name => Kind::Dir,
            0 | S_IFREG => Kind::File,
            S_IFDIR => Kind::Dir,
            S_IFLNK if !is_dir_name => Kind::Symlink,
            _ => return Err(unsafe_archive("unsupported entry type")),
        };
        if kind == Kind::Dir && size != 0 {
            return Err(unsafe_archive("a folder entry has data"));
        }
        let path = split_path(text)?;
        // Extended attributes and resource forks as macOS archivers store
        // them: in a `__MACOSX/` folder (Finder's Compress) or as `._name`
        // AppleDouble files beside each file (`ditto -c -k` without
        // `--norsrc`). Not part of the app, and inside a bundle they would
        // break its code signature; attributes are never restored.
        if path.first().is_some_and(|c| c == "__MACOSX")
            || path.last().is_some_and(|c| c.starts_with("._"))
        {
            continue;
        }
        total_size = total_size.saturating_add(size);
        if total_size > limits.max_total {
            return Err(unsafe_archive("unpacks to more than the limit"));
        }
        let key = path.join("/").to_ascii_lowercase();
        if seen.insert(key, kind).is_some() {
            return Err(unsafe_archive(
                "two entries with the same path (letter case aside)",
            ));
        }
        entries.push(Entry {
            path,
            kind,
            method,
            crc,
            compressed,
            size,
            executable: unix_mode & 0o111 != 0,
            data_start: 0,
            local_offset,
        });
    }

    // No entry may sit inside a file or a link.
    for e in &entries {
        for n in 1..e.path.len() {
            let prefix = e.path[..n].join("/").to_ascii_lowercase();
            match seen.get(&prefix) {
                Some(Kind::Symlink) => {
                    return Err(unsafe_archive("an entry's path runs through a link"));
                }
                Some(Kind::File) => {
                    return Err(unsafe_archive("an entry's path runs through a file"));
                }
                _ => {}
            }
        }
    }

    // Local headers: present, agreeing with the directory, data not
    // overlapping another entry or the directory.
    for e in &mut entries {
        let mut lh = [0u8; 30];
        if e.local_offset + 30 > cd_offset {
            return Err(unsafe_archive("local header out of bounds"));
        }
        read_at(file, e.local_offset, &mut lh)?;
        if u32_at(&lh, 0) != LOCAL_SIG {
            return Err(unsafe_archive("missing local header"));
        }
        if u16_at(&lh, 6) & 0x0041 != 0 || u16_at(&lh, 8) != e.method {
            return Err(unsafe_archive("local header disagrees with the directory"));
        }
        let (n, x) = (u64::from(u16_at(&lh, 26)), u64::from(u16_at(&lh, 28)));
        let mut lname = vec![0; n as usize];
        read_at(file, e.local_offset + 30, &mut lname)?;
        let lpath = std::str::from_utf8(&lname)
            .ok()
            .and_then(|t| split_path(t).ok());
        if lpath.as_ref() != Some(&e.path) {
            return Err(unsafe_archive("local header names another file"));
        }
        e.data_start = e.local_offset + 30 + n + x;
        if e.data_start
            .checked_add(e.compressed)
            .is_none_or(|end| end > cd_offset)
        {
            return Err(unsafe_archive("entry data out of bounds"));
        }
    }
    let mut spans: Vec<(u64, u64)> = entries
        .iter()
        .map(|e| (e.local_offset, e.data_start + e.compressed))
        .collect();
    spans.sort_unstable();
    if spans.windows(2).any(|w| w[1].0 < w[0].1) {
        return Err(unsafe_archive("entries overlap"));
    }
    // Files are written in the order their data lies in the archive.
    entries.sort_by_key(|e| e.local_offset);
    Ok(entries)
}

/// Splits and checks an entry name: `/` and `\` both separate; components
/// are printable ASCII without characters Windows treats specially, never
/// `.`, `..`, empty, a device name, or ending in a dot or space.
fn split_path(name: &str) -> Result<Vec<String>, UpdateError> {
    if name.len() > MAX_PATH {
        return Err(unsafe_archive("path too long"));
    }
    if name.bytes().any(|b| !(0x20..0x7f).contains(&b)) {
        return Err(unsafe_archive("file names must be printable ASCII"));
    }
    let normalized = name.replace('\\', "/");
    let trimmed = normalized.strip_suffix('/').unwrap_or(&normalized);
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return Err(unsafe_archive("absolute or empty path"));
    }
    trimmed.split('/').map(check_component).collect()
}

fn check_component(c: &str) -> Result<String, UpdateError> {
    if c.is_empty() || c == "." || c == ".." {
        return Err(unsafe_archive("empty, . or .. path component"));
    }
    if c.len() > MAX_COMPONENT {
        return Err(unsafe_archive("path component too long"));
    }
    if c.contains([':', '<', '>', '"', '|', '?', '*']) {
        return Err(unsafe_archive(
            "a character Windows reserves in a file name",
        ));
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return Err(unsafe_archive("a file name ending in a dot or space"));
    }
    let stem = c
        .split('.')
        .next()
        .unwrap_or(c)
        .trim_end()
        .to_ascii_uppercase();
    let device = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && stem.as_bytes()[3].is_ascii_digit());
    if device {
        return Err(unsafe_archive("a Windows device name"));
    }
    Ok(c.to_string())
}

/// Reads every link's target and checks that it stays inside the package,
/// resolving through the package's other links. Returns the targets in the
/// order of the link entries.
fn check_symlinks(file: &mut File, entries: &[Entry]) -> Result<Vec<String>, UpdateError> {
    let mut targets: HashMap<Vec<String>, Vec<String>> = HashMap::new();
    let mut texts = Vec::new();
    for e in entries.iter().filter(|e| e.kind == Kind::Symlink) {
        if cfg!(not(unix)) {
            return Err(unsafe_archive(
                "symbolic links are refused on this platform",
            ));
        }
        if e.size > MAX_SYMLINK_TARGET {
            return Err(unsafe_archive("link target too long"));
        }
        let mut data = Vec::new();
        copy_entry(file, e, &mut data, &Cancel::new())?;
        let text =
            String::from_utf8(data).map_err(|_| unsafe_archive("link target is not text"))?;
        if text.is_empty()
            || text.starts_with('/')
            || text.contains('\\')
            || text.contains(':')
            || text.bytes().any(|b| !(0x20..0x7f).contains(&b))
        {
            return Err(unsafe_archive("link target is absolute or malformed"));
        }
        let parts: Vec<String> = text.split('/').map(str::to_string).collect();
        if parts.iter().any(String::is_empty) {
            return Err(unsafe_archive("link target has an empty component"));
        }
        targets.insert(lowercase(&e.path), parts);
        texts.push((e.path.clone(), text));
    }
    for (path, _) in &texts {
        let mut steps = 0;
        let parent = lowercase(&path[..path.len() - 1]);
        resolve(&targets, parent, &targets[&lowercase(path)], &mut steps)?;
    }
    Ok(texts.into_iter().map(|(_, t)| t).collect())
}

fn lowercase(parts: &[String]) -> Vec<String> {
    parts.iter().map(|p| p.to_ascii_lowercase()).collect()
}

/// Resolves `target` relative to the folder `at` (both lowercase) within
/// the package's tree, following links the package contains. Fails if any
/// step leaves the package or the links nest too deeply.
fn resolve(
    links: &HashMap<Vec<String>, Vec<String>>,
    mut at: Vec<String>,
    target: &[String],
    steps: &mut usize,
) -> Result<Vec<String>, UpdateError> {
    for part in target {
        match part.as_str() {
            "." => {}
            ".." => {
                if at.pop().is_none() {
                    return Err(unsafe_archive("a link points outside the package"));
                }
            }
            name => {
                at.push(name.to_ascii_lowercase());
                if let Some(inner) = links.get(&at) {
                    *steps += 1;
                    if *steps > MAX_LINK_DEPTH {
                        return Err(unsafe_archive("links nest too deeply"));
                    }
                    at.pop();
                    at = resolve(links, at, inner, steps)?;
                }
            }
        }
    }
    Ok(at)
}

fn read_at(file: &mut File, offset: u64, buf: &mut [u8]) -> Result<(), UpdateError> {
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.read_exact(buf))
        .map_err(|_| unsafe_archive("truncated archive"))
}

/// Copies one entry's data to `out`, inflating and checking the size and
/// CRC-32 against the directory.
fn copy_entry(
    file: &mut File,
    e: &Entry,
    out: &mut dyn Write,
    cancel: &Cancel,
) -> Result<(), UpdateError> {
    file.seek(SeekFrom::Start(e.data_start))
        .map_err(|_| unsafe_archive("truncated archive"))?;
    let mut input = file.take(e.compressed);
    let mut crc = Crc32::new();
    let mut written = 0u64;
    let mut emit = |bytes: &[u8]| -> Result<(), UpdateError> {
        written += bytes.len() as u64;
        if written > e.size {
            return Err(unsafe_archive("an entry unpacks to more than its size"));
        }
        crc.update(bytes);
        out.write_all(bytes)
            .map_err(|err| io_error("writing a file", err))
    };
    let mut inbuf = vec![0u8; CHUNK];
    if e.method == 0 {
        loop {
            if cancel.is_canceled() {
                return Err(UpdateError::new(Reason::Canceled, "canceled"));
            }
            let n = input
                .read(&mut inbuf)
                .map_err(|_| unsafe_archive("truncated archive"))?;
            if n == 0 {
                break;
            }
            emit(&inbuf[..n])?;
        }
    } else {
        let mut state = InflateState::new_boxed(DataFormat::Raw);
        let mut outbuf = vec![0u8; CHUNK];
        let (mut start, mut end) = (0usize, 0usize);
        let mut input_done = false;
        loop {
            if cancel.is_canceled() {
                return Err(UpdateError::new(Reason::Canceled, "canceled"));
            }
            if start == end && !input_done {
                end = input
                    .read(&mut inbuf)
                    .map_err(|_| unsafe_archive("truncated archive"))?;
                start = 0;
                input_done = end == 0;
            }
            let flush = if input_done {
                MZFlush::Finish
            } else {
                MZFlush::None
            };
            let r = inflate(&mut state, &inbuf[start..end], &mut outbuf, flush);
            start += r.bytes_consumed;
            emit(&outbuf[..r.bytes_written])?;
            match r.status {
                Ok(MZStatus::StreamEnd) => break,
                Ok(MZStatus::Ok) | Err(MZError::Buf) => {
                    if input_done && start == end && r.bytes_written == 0 {
                        return Err(unsafe_archive("truncated deflate data"));
                    }
                }
                _ => return Err(unsafe_archive("invalid deflate data")),
            }
        }
    }
    if written != e.size {
        return Err(unsafe_archive("an entry's size does not match"));
    }
    if crc.finish() != e.crc {
        return Err(unsafe_archive("CRC mismatch"));
    }
    Ok(())
}

/// CRC-32 (IEEE), as zip uses it.
pub(crate) struct Crc32 {
    value: u32,
}

impl Crc32 {
    pub(crate) fn new() -> Crc32 {
        Crc32 { value: !0 }
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
        let table = TABLE.get_or_init(|| {
            let mut t = [0u32; 256];
            for (i, slot) in t.iter_mut().enumerate() {
                let mut c = i as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        0xedb8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
                *slot = c;
            }
            t
        });
        let mut c = self.value;
        for &b in bytes {
            c = table[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8);
        }
        self.value = c;
    }

    pub(crate) fn finish(&self) -> u32 {
        !self.value
    }
}

/// A zip of regular files, stored (not compressed), for tests and tools
/// that need a small valid package. Names use `/`.
pub fn write_stored(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let offset = out.len() as u32;
        let mut crc = Crc32::new();
        crc.update(data);
        let crc = crc.finish();
        let mut common = Vec::new();
        common.extend_from_slice(&20u16.to_le_bytes()); // version needed
        common.extend_from_slice(&0u16.to_le_bytes()); // flags
        common.extend_from_slice(&0u16.to_le_bytes()); // stored
        common.extend_from_slice(&0u16.to_le_bytes()); // time
        common.extend_from_slice(&0x21u16.to_le_bytes()); // date: 1980-01-01
        common.extend_from_slice(&crc.to_le_bytes());
        common.extend_from_slice(&(data.len() as u32).to_le_bytes());
        common.extend_from_slice(&(data.len() as u32).to_le_bytes());
        common.extend_from_slice(&(name.len() as u16).to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes()); // extra
        out.extend_from_slice(&LOCAL_SIG.to_le_bytes());
        out.extend_from_slice(&common);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        central.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // made by
        central.extend_from_slice(&common);
        central.extend_from_slice(&[0; 6]); // comment length, disk, internal attributes
        central.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&EOCD_SIG.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Paths of a staged tree, for tests and diagnostics: every file, folder
/// and link below `root`, relative, with `/` separators, sorted.
pub fn list_tree(root: &Path) -> std::io::Result<Vec<String>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let ty = entry.file_type()?;
            if ty.is_dir() {
                out.push(format!("{rel}/"));
                walk(root, &path, out)?;
            } else {
                out.push(rel);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_zips_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("t.zip");
        std::fs::write(&zip, write_stored(&[("a/b.txt", b"bee"), ("c.txt", b"")])).unwrap();
        let dest = dir.path().join("out");
        extract(&zip, &dest, Limits::default(), &Cancel::new()).unwrap();
        assert_eq!(list_tree(&dest).unwrap(), ["a/", "a/b.txt", "c.txt"]);
        assert_eq!(std::fs::read(dest.join("a/b.txt")).unwrap(), b"bee");
    }

    #[test]
    fn crc_matches_the_reference_value() {
        let mut c = Crc32::new();
        c.update(b"123456789");
        assert_eq!(c.finish(), 0xcbf4_3926);
    }

    #[test]
    fn path_rules() {
        assert_eq!(
            split_path("NVRView.app/Contents/").unwrap(),
            ["NVRView.app", "Contents"]
        );
        assert_eq!(split_path(r"a\b.dll").unwrap(), ["a", "b.dll"]);
        for bad in [
            "/etc/passwd",
            "\\x",
            "../x",
            "a/../../x",
            "a/./b",
            "a//b",
            "C:/x",
            "a/b:stream",
            "a/CON",
            "a/nul.txt",
            "a/com1",
            "a/lpt9.log",
            "a/b.",
            "a/b ",
            "a/\u{e9}",
            "a/\tb",
            "",
            "/",
        ] {
            assert!(split_path(bad).is_err(), "{bad:?}");
        }
        assert!(split_path("a/console.txt").is_ok());
        assert!(split_path("a/com10").is_ok());
    }

    fn links(pairs: &[(&str, &str)]) -> HashMap<Vec<String>, Vec<String>> {
        pairs
            .iter()
            .map(|(p, t)| {
                (
                    p.split('/').map(str::to_string).collect(),
                    t.split('/').map(str::to_string).collect(),
                )
            })
            .collect()
    }

    fn check(
        map: &HashMap<Vec<String>, Vec<String>>,
        link: &str,
    ) -> Result<Vec<String>, UpdateError> {
        let path: Vec<String> = link.split('/').map(str::to_string).collect();
        let mut steps = 0;
        resolve(
            map,
            path[..path.len() - 1].to_vec(),
            &map[&path],
            &mut steps,
        )
    }

    #[test]
    fn link_resolution() {
        // A framework's usual links resolve inside.
        let m = links(&[
            ("f.framework/versions/current", "a"),
            ("f.framework/f", "versions/current/f"),
            ("f.framework/resources", "versions/current/resources"),
        ]);
        assert_eq!(
            check(&m, "f.framework/f").unwrap(),
            ["f.framework", "versions", "a", "f"]
        );
        assert!(check(&m, "f.framework/resources").is_ok());
        // Out by ..; out through another link that points at the root.
        let m = links(&[("a/up", "../.."), ("b", "."), ("c", "b/..")]);
        assert!(check(&m, "a/up").is_err());
        assert!(check(&m, "c").is_err());
        assert_eq!(check(&m, "b").unwrap(), Vec::<String>::new());
        // Loops end.
        let m = links(&[("x", "y"), ("y", "x")]);
        assert!(check(&m, "x").is_err());
    }
}
