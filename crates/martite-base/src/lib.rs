//! Foundational types shared by every Martite crate.

pub mod diagnostic;
pub mod source;

pub use diagnostic::{
    Applicability, Code, Diagnostic, Fix, FixEdit, LintGroup, MAX_DIAGNOSTICS, Severity,
    cap_diagnostics, cap_diagnostics_at,
};
pub use source::{
    FileSystemProvider, LoadLimits, MemoryProvider, SourceFile, SourceId, SourceMap,
    SourceProvider, Span, normalize_source,
};
