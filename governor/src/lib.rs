//! InvoiceShield governor: a fail-closed pre-payment gate for AI accounts-payable agents.
//! The model proposes; this code decides. Payee always from the vendor master.
pub mod audit;
pub mod engine;
pub mod ledger;
pub mod master;
pub mod master_change;
pub mod normalize;
pub mod verdict;

pub use engine::{Decision, Evidence, Governor, Proposal, Request};
pub use master::Master;
pub use verdict::{merge, parse_checker_signals, Severity, Signal, Verdict};
