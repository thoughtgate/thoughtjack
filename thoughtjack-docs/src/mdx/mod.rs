//! MDX page generation for `ThoughtJack` scenarios.
//!
//! Converts parsed scenario configurations into MDX pages with:
//! - YAML frontmatter for Docusaurus
//! - Mermaid diagram code blocks
//! - Human-readable prose for structural elements
//! - Detection guidance and framework mappings
//!
//! TJ-SPEC-011 F-003

pub mod frontmatter;
pub mod page;
pub mod prose;
