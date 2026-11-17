//! Reports on a run.
//!
//! A run says everything it does as events
//! (`meridian_orchestrator::event`), and those events are the only input
//! here. A [`Report`] is a fold over them, so the same report comes out of a
//! run as it happens, a run read back from the state, and a file of events
//! carried in from another machine. Nothing in this crate looks at a
//! document, an artifact or a tool's output, which is what keeps a secret
//! out of a report: an event never holds one.
//!
//! There are two ways out:
//!
//! - [`render`] turns a report into a file a person or a CI system reads:
//!   JSON, JUnit XML, Markdown or one self-contained HTML page.
//! - [`sink`] sends the events themselves, as they happen, to a program that
//!   forwards them somewhere else: a webhook, a chat channel, a collector.
//!
//! The design is `docs/plans/reporting.md`.

mod model;
pub mod render;
pub mod sink;
mod time;

pub use model::{Phase, Report, Resource, RunState, Step, StepOutput, StepState, Summary};
pub use render::{render, Format};
