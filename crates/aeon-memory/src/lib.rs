//! AEON local memory and retrieval.
//!
//! Everything runs on the device; nothing leaves it.
//!
//! * [`lexical`]: BM25 keyword index.
//! * [`vector`]: cosine similarity over embeddings.
//! * [`graph`]: knowledge graph with spreading activation (related ideas
//!   light up from a few seeds).
//! * [`fusion`]: reciprocal-rank fusion of several ranked lists.
//! * [`chunk`]: recursive text chunking with overlap and source offsets, to feed the indexes.
#![forbid(unsafe_code)]

pub mod chunk;
pub mod context;
pub mod fusion;
pub mod graph;
pub mod lexical;
pub mod vector;

/// Document id shared by all indexes.
pub type DocId = u64;
