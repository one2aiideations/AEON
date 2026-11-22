//! AEON local memory and retrieval.
//!
//! Everything runs on the device; nothing leaves it.
//!
//! * [`lexical`]: BM25 keyword index.
//! * [`vector`]: cosine similarity over embeddings.
//! * [`graph`]: knowledge graph with spreading activation (related ideas
//!   light up from a few seeds).
//! * [`fusion`]: reciprocal-rank fusion of several ranked lists.
//! * [`ontology`]: class hierarchy (is-a) with inherited, typed relations.
#![forbid(unsafe_code)]

pub mod fusion;
pub mod graph;
pub mod lexical;
pub mod ontology;
pub mod vector;

/// Document id shared by all indexes.
pub type DocId = u64;
