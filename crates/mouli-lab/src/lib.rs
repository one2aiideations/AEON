//! MOULI-LAB: a standard-library-only research harness for the Aetheron dendritic model.
//!
//! This is research code, kept apart from the product crates. Every hand-written backward pass is
//! checked against finite differences by `mouli-lab selftest`. Nothing here is tuned to reproduce
//! a published number; the benchmark reports what it measures, including the cases where the
//! Aetheron model does not beat a plain MLP.
#![forbid(unsafe_code)]

pub type F = f64;

pub mod aetheron_ml;
pub mod baselines;
pub mod continual;
pub mod core;
pub mod data;
pub mod linalg;
pub mod report;
pub mod rng;
pub mod temporal;
