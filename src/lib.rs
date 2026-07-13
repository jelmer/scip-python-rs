mod document;
mod infer;
mod line_index;
mod metadata;
mod project;
mod symbols;

pub use metadata::{ProjectMetadata, project_metadata};
pub use project::{IndexResult, Options, ParseFailure, index_project};
