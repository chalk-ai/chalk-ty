#![warn(
    clippy::disallowed_methods,
    reason = "Prefer System trait methods over std methods in ty crates"
)]

mod active_project;
mod call_matcher;
mod diagnostic;
mod facts;
mod project;
mod reachability;
mod supported_functions;
mod suppression;
mod type_matcher;

pub use active_project::{ActiveChalkProject, ChalkProjectInput};
pub use call_matcher::CallNoMatchReason;
pub use diagnostic::{
    ChalkDiagnostic, ChalkDiagnosticKind, ChalkDiagnosticSeverity, UnsupportedFunctionDetails,
    UnsupportedTargetDetail, chalk_diagnostics_for_file,
};
pub use project::{ChalkProject, ChalkProjectError, discover_chalk_project};
