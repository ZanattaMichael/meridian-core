//! `meridian test`: tests a person writes for their own documents and
//! modules.
//!
//! A `Test` document names the documents under test and a list of cases.
//! Each case gives the data the documents are compiled or planned with,
//! and says what should come out: which document compiles, which is refused
//! and under which rule, what the emitted files hold, what a plan would
//! change. The checks are structured expectations plus expressions in the
//! one expression language, so a test is a document, not a script.
//!
//! Three levels. A **compile** case needs no tool: the documents are
//! compiled by their plugins and the files looked at. A **plan** case asks
//! each document's tool what applying would change, from a scratch state
//! directory that starts empty, so it needs the tool, or a stand-in named
//! in the project's settings. A **lifecycle** case applies to a scratch
//! environment, checks it, and destroys it, each phase reported on its own.

mod assert;
mod document;
mod junit;
mod lab;
mod run;
mod sweep;

pub use assert::stage_value;
pub use document::{
    discover, load, Assertion, Case, ChangeExpect, Expect, FileExpect, Phases, Run, TestFile,
    TEST_FILE_SUFFIX, TEST_KIND,
};
pub use junit::junit;
pub use lab::{
    cells, load_lab, new_lab_run_id, run_lab, Cell, CellResult, LabFile, DEFAULT_EXPIRY, LAB_KIND,
    LAST_RUN_FILE,
};
pub use run::{
    run_file, CaseResult, FileResult, PhaseResult, Resolver, Runner, StageSummary, Status,
    TEST_RUN_ID,
};
pub use sweep::{environments, now, sweep, Record, SweepOutcome, Swept, RECORD_FILE};
