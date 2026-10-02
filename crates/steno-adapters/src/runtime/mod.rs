//! The delivery runtime: the coordinator every pipeline run hands a meeting
//! to, and the ledger that holds the policy.

mod coordinator;
mod ledger;

pub use coordinator::{DeliveryCoordinator, DestinationFactory};
pub use ledger::{DeliveryLedger, receipt_folder_path};
