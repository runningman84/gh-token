//! gh-token-server: issues short-lived GitHub App installation tokens to
//! in-cluster clients while keeping the GitHub App credentials private.

pub mod app;
pub mod config;
pub mod github;
pub mod http;
pub mod metrics;
pub mod process;
pub mod ratelimit;
pub mod store;
