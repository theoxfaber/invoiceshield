//! InvoiceShield governor: a fail-closed pre-payment gate for AI accounts-payable agents.
//! The model proposes; this code decides. Payee always from the vendor master.
pub mod anchor;
pub mod audit;
pub mod auth;
pub mod config;
pub mod decisions;
pub mod engine;
pub mod ledger;
pub mod lines;
pub mod master;
pub mod master_change;
pub mod master_store;
pub mod normalize;
pub mod observability;
pub mod persist;
pub mod ratelimit;
pub mod review_queue;
pub mod secrets;
pub mod security;
pub mod tls;
pub mod trust;
pub mod verdict;

pub use engine::{Decision, Evidence, Governor, Proposal, Request};
pub use master::Master;
pub use trust::{TrustConfig, TrustVerifier};
pub use verdict::{merge, parse_checker_signals, Severity, Signal, Verdict};
