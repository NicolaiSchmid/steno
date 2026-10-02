//! Where uploads live until the intake takes them, how chunks land in the
//! partial file, and what a phone may announce.

pub mod inbox;
pub mod metadata_validation;
pub mod receiving_file;

pub use inbox::Inbox;
pub use metadata_validation::MetadataValidation;
