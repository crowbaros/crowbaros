// SPDX-License-Identifier: MIT
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const MAX_BYTES: u64 = 32 * 1024 * 1024;

pub fn read(path: &Path) -> io::Result<String> {
    if !fs::metadata(path)?.is_file() {
        return Err(io::Error::other("Choose a regular file"));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(io::Error::other("Choose a regular file up to 32 MiB"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES || bytes.contains(&0) {
        return Err(io::Error::other("Expected a text file up to 32 MiB"));
    }
    String::from_utf8(bytes).map_err(|_| io::Error::other("Expected UTF-8 text"))
}

pub fn save(path: &Path, text: &str) -> io::Result<()> {
    if text.len() as u64 > MAX_BYTES || text.contains('\0') {
        return Err(io::Error::other("Save UTF-8 text up to 32 MiB without NUL bytes"));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let permissions = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Some(meta.permissions()),
        Ok(_) => return Err(io::Error::other("Save to a regular file")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(text.as_bytes())?;
    if let Some(mode) = permissions {
        temporary.as_file().set_permissions(mode)?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    File::open(parent)?.sync_all()
}

pub fn cli_paths(args: impl Iterator<Item = std::ffi::OsString>) -> Vec<std::path::PathBuf> {
    args.map(std::path::PathBuf::from).collect()
}

pub fn language(path: Option<&Path>) -> &'static str {
    match path.and_then(Path::extension).and_then(|ext| ext.to_str()) {
        Some("md" | "markdown") => "markdown",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        Some("sh" | "bash") => "bash",
        Some("rs") => "rust",
        Some("py") => "python",
        _ => "text",
    }
}

/// Return a match in the UTF-16 coordinates used by the native input handler.
pub fn next_match(text: &str, query: &str, cursor: usize) -> Option<std::ops::Range<usize>> {
    if query.is_empty() {
        return None;
    }
    let start = text[cursor..]
        .find(query)
        .map(|i| i + cursor)
        .or_else(|| text.find(query))?;
    let start = text[..start].encode_utf16().count();
    Some(start..start + query.encode_utf16().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_leaves_old_open_descriptor_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note");
        save(&path, "first").unwrap();
        let mut original = File::open(&path).unwrap();
        save(&path, "second").unwrap();
        let mut text = String::new();
        original.read_to_string(&mut text).unwrap();
        assert_eq!(text, "first");
        assert_eq!(read(&path).unwrap(), "second");
    }

    #[test]
    fn refuses_binary_data_and_special_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binary");
        fs::write(&path, b"a\0b").unwrap();
        assert!(read(&path).is_err());
        let file = File::create(dir.path().join("large")).unwrap();
        file.set_len(MAX_BYTES + 1).unwrap();
        assert!(read(&dir.path().join("large")).is_err());
        assert!(read(Path::new("/dev/null")).is_err());
        assert!(save(dir.path(), "text").is_err());
    }

    #[test]
    fn cli_preserves_each_path_including_non_utf8_names() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let args = [OsString::from("two words.md"), OsString::from_vec(vec![0xff])];
        assert_eq!(cli_paths(args.clone().into_iter()), args.map(std::path::PathBuf::from));
        assert!(cli_paths(std::iter::empty()).is_empty());
    }

    #[test]
    fn find_wraps_and_returns_native_input_offsets() {
        assert_eq!(next_match("🌱abc abc", "abc", 7), Some(6..9));
        assert_eq!(next_match("🌱abc abc", "abc", 11), Some(2..5));
        assert_eq!(next_match("🌱", "", 0), None);
    }
}
