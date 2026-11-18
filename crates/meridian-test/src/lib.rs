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
//! Two levels exist today. A **compile** case needs no tool: the documents
//! are compiled by their plugins and the files looked at. A **plan** case
//! asks each document's tool what applying would change, from a scratch
//! state directory that starts empty, so it needs the tool, or a stand-in
//! named in the project's settings. Applying to something disposable is
//! not a level yet; see the reference page.

mod assert;
mod document;
mod junit;
mod run;

pub use assert::stage_value;
pub use document::{
    discover, load, Assertion, Case, ChangeExpect, Expect, FileExpect, Run, TestFile,
    TEST_FILE_SUFFIX, TEST_KIND,
};
pub use junit::junit;
pub use run::{run_file, CaseResult, FileResult, Resolver, Runner, StageSummary, Status};
