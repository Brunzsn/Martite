use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

/// One loaded source file.
#[derive(Debug, Clone)]
pub struct SourceFile {
    name: String,
    text: String,
    line_starts: Vec<u32>,
}

impl SourceFile {
    fn new(name: String, text: String) -> Self {
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
        let offset = offset.min(self.text.len() as u32);
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let line_start = self.line_starts[line] as usize;
        let col = self.text[line_start..offset as usize].chars().count() + 1;
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
        let start = span.start.min(self.text.len() as u32) as usize;
        let end = span.end.min(self.text.len() as u32) as usize;
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
        self.file(id).line_col(offset)
    }
}

/// The interpreter never reads files itself; it asks a provider. The CLI uses
/// [`FileSystemProvider`], embeds can use [`MemoryProvider`] or anything else.
pub trait SourceProvider {
    /// Read a path relative to the project root. Paths use `/` separators.
    fn read(&self, path: &str) -> Result<String, String>;
}

/// Reads modules from a directory on disk.
#[derive(Debug, Clone)]
pub struct FileSystemProvider {
    root: PathBuf,
}

impl FileSystemProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl SourceProvider for FileSystemProvider {
    fn read(&self, path: &str) -> Result<String, String> {
        let relative = path.replace('\\', "/");
        if relative
            .split('/')
            .any(|part| part == ".." || part.is_empty())
        {
            return Err(format!("invalid module path `{path}`"));
        }
        let full = self.root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
        std::fs::read_to_string(&full).map_err(|e| format!("cannot read {}: {e}", full.display()))
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

    #[test]
    fn line_and_scalar_column_are_one_based() {
        let file = SourceFile::new("x.mt".into(), "a\nbé\n".into());
        assert_eq!(file.line_col(0), (1, 1));
        assert_eq!(file.line_col(2), (2, 1));
        assert_eq!(file.line_col(5), (2, 3));
    }

    #[test]
    fn span_merge() {
        assert_eq!(
            Span::new(2, 4).merge(Span::new(7, 9)),
            Span::new(2, 9)
        );
    }
}
