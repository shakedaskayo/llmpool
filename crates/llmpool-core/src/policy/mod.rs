pub mod conditions;
pub mod context;
pub mod engine;
pub mod file;
pub mod rule;
pub mod select;
pub mod triggers;
pub mod validate;

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("{0}")]
    Parse(String),
    #[error("{}", .0.join("; "))]
    Invalid(Vec<String>),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
