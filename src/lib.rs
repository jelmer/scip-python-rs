mod document;
mod infer;
mod line_index;
mod metadata;
mod project;
mod symbols;
mod syntax;

pub use metadata::{ProjectMetadata, project_metadata};
pub use project::{IndexResult, Options, ParseFailure, index_project};
