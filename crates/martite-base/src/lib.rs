//! Foundational types shared by every Martite crate.

pub mod diagnostic;
pub mod source;

pub use diagnostic::{Applicability, Code, Diagnostic, Emit, Fix, FixEdit, LintGroup, Severity};
pub use source::{
    FileSystemProvider, LoadLimits, MemoryProvider, SourceFile, SourceId, SourceMap,
    SourceProvider, Span, normalize_source,
};
