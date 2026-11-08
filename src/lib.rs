//! A durable, tool-independent archive of AI agent conversations.
//!
//! Two layers with opposite change rates: Captures (verbatim native payloads,
//! schema-free, the only source of truth) and Conversations (the narrow
//! canonical model). See `SPEC.md` for requirements and `CONTEXT.md` for the
//! ubiquitous language used throughout this crate.

pub mod activity;
pub mod adapters;
pub mod archive;
pub mod assignments;
pub mod bundle;
pub mod chatgpt;
pub mod cli;
pub mod dialog;
pub mod digest;
pub mod fleet;
pub mod info;
pub mod leads;
mod markdown;
pub mod media;
pub mod model;
mod name_tables;
pub mod origins;
mod path_key;
pub mod pull;
pub mod push_titles;
pub mod render;
mod repository_key;
pub mod retitle;
pub mod session_day;
pub mod timefmt;
pub mod workday;

pub mod repository_resolution;

mod bounded_process;
