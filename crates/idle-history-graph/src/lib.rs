//! Portable activity ordering, causal lanes and retained graph routes.
//!
//! Native history queries and browser clients share these indexed algorithms.
//! Callers supply recorded relationships and consume abstract rows and lanes.

mod layout;
pub mod live;
