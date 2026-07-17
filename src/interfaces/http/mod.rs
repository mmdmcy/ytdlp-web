//! Axum routes, handlers, component status, and HTML presentation.

mod component_status;
mod downloads;
pub(crate) mod presentation;
mod router;

pub(crate) use router::build_router;
