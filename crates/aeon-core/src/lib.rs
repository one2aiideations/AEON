//! AEON object model.
//!
//! Everything the AI-native layer manages is a typed [`object::KernelObject`]
//! (users, agents, models, capabilities, workflows, memories, digital
//! twins) with labels, owner, versioned state and relationships.
//!
//! Design rule: the core does not know what an LLM is. Reasoning backends
//! sit behind the [`engine::CognitiveEngine`] trait, tools behind the
//! [`capability::CapabilityRegistry`], and agents follow the
//! [`agent::Agent`] lifecycle (observe, plan, act, reflect).
#![forbid(unsafe_code)]

pub mod agent;
pub mod capability;
pub mod engine;
pub mod object;
