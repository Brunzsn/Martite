//! Foundational types shared by every Martite crate.

pub mod diagnostic;
pub mod source;

pub use diagnostic::{Code, Diagnostic, Emit, Severity};
pub use source::{
    FileSystemProvider, LoadLimits, MemoryProvider, SourceFile, SourceId, SourceMap,
    SourceProvider, Span,
};
