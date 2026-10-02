//! Sourcer server: API, background worker and scheduler for resourcing and outreach.
//! See docs/SRS.md for requirements and docs/README.md for the design.

pub mod admin;
pub mod ai;
pub mod app;
pub mod audit;
pub mod auth;
pub mod candidates;
pub mod config;
pub mod crm;
pub mod cv;
pub mod db;
pub mod domain;
pub mod employer;
pub mod jobs;
pub mod outreach;
pub mod people;
pub mod plan;
pub mod policy;
pub mod ratelimit;
pub mod recruitly;
pub mod retune;
pub mod roles;
pub mod search;
pub mod searching;
pub mod sources;
pub mod team;
pub mod worker;

#[cfg(test)]
pub(crate) mod testutil;
