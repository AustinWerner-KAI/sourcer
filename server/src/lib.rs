//! Sourcer server: API, background worker and scheduler for resourcing and outreach.
//! See docs/SRS.md for requirements and docs/README.md for the design.

pub mod app;
pub mod audit;
pub mod config;
pub mod db;
pub mod domain;
pub mod jobs;
pub mod policy;
pub mod search;
pub mod sources;
pub mod worker;

#[cfg(test)]
pub(crate) mod testutil;
