use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use cap_std::ambient_authority;
use cap_std::fs::Dir;

/// Identifies one source file inside a [`SourceMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(pub u32);

/// A half-open byte range inside a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    pub const fn empty(at: u32) -> Self {
        Self { start: at, end: at }
    }

    pub fn merge(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn contains(self, other: Span) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }
}

/// Normalize source text before it is stored or lexed.
///
/// A leading BOM is removed and CRLF is normalized to LF, as required by the
/// language rules. Keeping this in one place means token spans always refer to
/// the same bytes that a [`SourceMap`] serves to diagnostics.
pub fn normalize_source(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.contains("\r\n") {
        text.replace("\r\n", "\n")
    } else {
        text.to_string()
    }
}

/// One loaded source file.
#[derive(Debug, Clone)]
pub struct SourceFile {
    name: String,
    text: String,
    line_starts: Vec<u32>,
}

impl SourceFile {
    fn new(name: String, text: String) -> Self {
        let text = normalize_source(&text);
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push((i + 1) as u32);
            }
        }
        Self {
            name,
            text,
            line_starts,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// 1-based line and 1-based Unicode-scalar column.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let mut offset = offset.min(self.text.len() as u32) as usize;
        while offset > 0 && !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = match self.line_starts.binary_search(&(offset as u32)) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let line_start = self.line_starts[line] as usize;
        let col = self.text[line_start..offset].chars().count() + 1;
        ((line + 1) as u32, col as u32)
    }

    pub fn line_text(&self, line: u32) -> &str {
        let idx = line.saturating_sub(1) as usize;
        let Some(&start) = self.line_starts.get(idx) else {
            return "";
        };
        let end = self
            .line_starts
            .get(idx + 1)
            .copied()
            .unwrap_or(self.text.len() as u32);
        self.text[start as usize..end as usize].trim_end_matches(['\r', '\n'])
    }

    pub fn slice(&self, span: Span) -> &str {
        let mut start = span.start.min(self.text.len() as u32) as usize;
        let mut end = span.end.min(self.text.len() as u32) as usize;
        while start > 0 && !self.text.is_char_boundary(start) {
            start -= 1;
        }
        while end > 0 && !self.text.is_char_boundary(end) {
            end -= 1;
        }
        if start <= end {
            &self.text[start..end]
        } else {
            ""
        }
    }
}

/// All sources seen during one compilation.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, name: impl Into<String>, text: impl Into<String>) -> SourceId {
        let id = SourceId(self.files.len() as u32);
        self.files.push(SourceFile::new(name.into(), text.into()));
        id
    }

    pub fn get(&self, id: SourceId) -> Option<&SourceFile> {
        self.files.get(id.0 as usize)
    }

    /// Panics if `id` was not produced by this map. Prefer [`SourceMap::get`]
    /// when the ID came from an untrusted or external source.
    pub fn file(&self, id: SourceId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn files(&self) -> impl Iterator<Item = (SourceId, &SourceFile)> {
        self.files
            .iter()
            .enumerate()
            .map(|(i, f)| (SourceId(i as u32), f))
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn line_col(&self, id: SourceId, offset: u32) -> (u32, u32) {
        self.get(id)
            .map(|file| file.line_col(offset))
            .unwrap_or((1, 1))
    }
}

/// The interpreter never reads files itself; it asks a provider. The CLI uses
/// [`FileSystemProvider`], embeds can use [`MemoryProvider`] or anything else.
pub trait SourceProvider {
    /// Read a path relative to the project root. Paths use `/` separators.
    fn read(&self, path: &str) -> Result<String, String>;

    /// Read a path while refusing to load more than `max_bytes`.
    ///
    /// The default implementation exists for simple providers, but filesystem
    /// providers should override it so an oversized file is rejected before it
    /// is read into memory.
    fn read_limited(&self, path: &str, max_bytes: usize) -> Result<String, String> {
        let text = self.read(path)?;
        if text.len() > max_bytes {
            Err(format!("file `{path}` exceeds the {max_bytes} byte limit"))
        } else {
            Ok(text)
        }
    }
}

/// How a [`FileSystemProvider`] is allowed to resolve paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FsMode {
    /// Relative paths only, rooted at the configured directory.
    Sandboxed,
    /// Paths are resolved with the host process's filesystem authority.
    Unrestricted,
}

/// Reads modules from a directory on disk.
///
/// The default constructor creates a sandboxed provider: module paths are
/// opened relative to `root`, `..`/absolute paths are rejected, and symlink
/// escapes are refused by the capability-based filesystem API.
///
/// Use [`FileSystemProvider::unrestricted`] for the standalone CLI host, or
/// when an embedding deliberately wants to grant ambient filesystem access.
#[derive(Debug, Clone)]
pub struct FileSystemProvider {
    root: PathBuf,
    mode: FsMode,
}

impl FileSystemProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            mode: FsMode::Sandboxed,
        }
    }

    /// Create a provider that resolves paths using ambient filesystem
    /// authority. This is intentionally explicit.
    pub fn unrestricted(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            mode: FsMode::Unrestricted,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_sandboxed(&self) -> bool {
        self.mode == FsMode::Sandboxed
    }
}

fn is_safe_relative_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    if normalized.is_empty() {
        return false;
    }
    normalized.split('/').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.contains(':')
            && !part.contains('\0')
    })
}

fn read_bounded(mut file: impl Read, max_bytes: usize) -> Result<String, String> {
    let mut text = String::new();
    let limit = (max_bytes as u64).saturating_add(1);
    file.by_ref()
        .take(limit)
        .read_to_string(&mut text)
        .map_err(|e| format!("cannot read source: {e}"))?;
    if text.len() > max_bytes {
        return Err(format!("source file exceeds the {max_bytes} byte limit"));
    }
    Ok(text)
}

impl SourceProvider for FileSystemProvider {
    fn read(&self, path: &str) -> Result<String, String> {
        self.read_limited(path, usize::MAX)
    }

    fn read_limited(&self, path: &str, max_bytes: usize) -> Result<String, String> {
        match self.mode {
            FsMode::Sandboxed => {
                if !is_safe_relative_path(path) {
                    return Err(format!("invalid module path `{path}`"));
                }
                let relative = path.replace('\\', "/");
                let dir = Dir::open_ambient_dir(&self.root, ambient_authority())
                    .map_err(|e| format!("cannot open {}: {e}", self.root.display()))?;
                let file = dir.open(&relative).map_err(|e| {
                    format!("cannot read {}: {e}", self.root.join(&relative).display())
                })?;
                read_bounded(file, max_bytes)
            }
            FsMode::Unrestricted => {
                let relative = path.replace('\\', "/");
                let full = self
                    .root
                    .join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
                let file = std::fs::File::open(&full)
                    .map_err(|e| format!("cannot read {}: {e}", full.display()))?;
                read_bounded(file, max_bytes)
            }
        }
    }
}

/// An in-memory provider handy for tests and embedding.
#[derive(Debug, Clone, Default)]
pub struct MemoryProvider {
    files: HashMap<String, String>,
}

impl MemoryProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, path: impl Into<String>, text: impl Into<String>) -> &mut Self {
        self.files.insert(path.into(), text.into());
        self
    }

    pub fn contains(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }
}

impl SourceProvider for MemoryProvider {
    fn read(&self, path: &str) -> Result<String, String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| format!("file not found: {path}"))
    }

    fn read_limited(&self, path: &str, max_bytes: usize) -> Result<String, String> {
        let text = self.read(path)?;
        if text.len() > max_bytes {
            Err(format!("file `{path}` exceeds the {max_bytes} byte limit"))
        } else {
            Ok(text)
        }
    }
}

/// Load limits that keep hostile input from exhausting the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadLimits {
    pub file_size: usize,
    pub nesting_depth: usize,
    pub file_count: usize,
}

impl Default for LoadLimits {
    fn default() -> Self {
        Self {
            file_size: 1024 * 1024,
            nesting_depth: 128,
            file_count: 256,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn line_and_scalar_column_are_one_based() {
        let file = SourceFile::new("x.mt".into(), "a\nbé\n".into());
        assert_eq!(file.line_col(0), (1, 1));
        assert_eq!(file.line_col(2), (2, 1));
        assert_eq!(file.line_col(5), (2, 3));
    }

    #[test]
    fn non_boundary_offsets_are_clamped() {
        let file = SourceFile::new("x.mt".into(), "aé\n".into());
        assert_eq!(file.line_col(2), (1, 2));
        assert_eq!(file.slice(Span::new(0, 2)), "a");
    }

    #[test]
    fn bom_and_crlf_are_normalized() {
        let file = SourceFile::new("x.mt".into(), "\u{feff}a\r\nb".into());
        assert_eq!(file.text(), "a\nb");
        assert_eq!(file.line_col(2), (2, 1));
    }

    #[test]
    fn bounded_read_stops_at_limit() {
        let err = read_bounded(Cursor::new(b"abcdef".to_vec()), 3).unwrap_err();
        assert!(err.contains("exceeds"));
    }

    #[test]
    fn relative_paths_are_checked() {
        assert!(is_safe_relative_path("a/b.mt"));
        assert!(!is_safe_relative_path("../a.mt"));
        assert!(!is_safe_relative_path("C:/a.mt"));
        assert!(!is_safe_relative_path("/a.mt"));
        assert!(!is_safe_relative_path("a//b.mt"));
    }

    #[test]
    fn sandboxed_provider_rejects_escape_attempts() {
        let provider = FileSystemProvider::new(".");
        assert!(provider.read_limited("../outside.mt", 1024).is_err());
        assert!(provider.read_limited("C:/outside.mt", 1024).is_err());
        assert!(provider.is_sandboxed());
        let provider = FileSystemProvider::unrestricted(".");
        assert!(!provider.is_sandboxed());
    }

    #[test]
    fn span_merge() {
        assert_eq!(Span::new(2, 4).merge(Span::new(7, 9)), Span::new(2, 9));
    }
}
