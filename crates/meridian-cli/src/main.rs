//! The `meridian` command.
//!
//! It is a thin shell: it reads documents, loads plugins, and prints what the
//! orchestrator reports. Stdout carries results, stderr carries the tools'
//! own output and errors, and the exit status says whether everything asked
//! for was done.
//!
//! Everything written to stdout goes through [`Stdout`], so that a reader
//! which stops early (`meridian plugins | head`) ends the printing, not the
//! command: what remains to be done is done, and the exit status is the
//! command's own.

/// Prints a line to stdout; see [`Stdout`].
macro_rules! outln {
    ($($arg:tt)*) => {
        $crate::Stdout.line(format_args!($($arg)*))
    };
}

/// Prints to stdout without a newline; see [`Stdout`].
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::Stdout.text(format_args!($($arg)*))
    };
}

mod args;
mod commands;
mod import;
mod interrupt;
mod output;
mod print;
mod tools;

use std::collections::BTreeMap;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use meridian_ir::Bundle;
use meridian_orchestrator::{
    describe_duration, lock, record, run_selected, selected, Backend, Decisions, Drift, Event,
    ExecBackend, LocalBackend, LockInfo, Mode, Observer, OnFailure, Options, Outcome, Policy,
    Reason, Refusal, Resume, RollbackOutcome, RollbackReport, RunRecord, RunReport, Selector,
    StageId, StageReport, Stop, MASK,
};
use meridian_plugin_host::{
    signature::signature_path, Policy as SignaturePolicy, PublicKey, Registry, SecretKey,
    Signature, StartOptions, Verification,
};
use meridian_report::sink::{self, Dispatcher};
use meridian_report::{Format, Report};
use meridian_sdk::{ChangeAction, Data, LogLine, LogStream, Output};

use args::*;
use commands::*;
use output::*;
use print::*;
pub(crate) use print::{count, Stdout};

#[derive(Parser)]
#[command(
    name = "meridian",
    version,
    about = "One document, any tool: compile Meridian documents and run the tools they target"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    #[command(flatten)]
    trust: Trust,
    /// How the result is printed: text for a person, or json, one JSON
    /// object per command, for a pipeline or a wrapping tool. The report
    /// for people then moves to standard error
    #[arg(
        long,
        global = true,
        value_name = "FORMAT",
        env = "MERIDIAN_OUTPUT",
        default_value = "text"
    )]
    output: OutputFormat,
}

fn main() -> ExitCode {
    // Before anything is read: no core dump of this process, and no other
    // process of the same user reading its memory, so a secret fetched later
    // cannot leave that way.
    let _ = meridian_secret::harden_process();
    let cli = Cli::parse();
    settle_output(cli.output, &cli.command.name());
    let outcome = settle(cli.trust).and_then(|()| match cli.command {
        Command::Validate { files, state_dir } => load(&files, &state_dir).map(|bundle| {
            if json_output() {
                emit(
                    true,
                    body([
                        ("documents", bundle.documents.len().into()),
                        ("diagnostics", serde_json::json!([])),
                    ]),
                );
            } else {
                outln!("{} valid", count(bundle.documents.len(), "document"));
            }
        }),
        Command::Module { command, state_dir } => module(command, &state_dir),
        Command::Secret {
            command: SecretCommand::Check { files, state_dir },
        } => check_secrets(&files, &state_dir),
        Command::Secret {
            command:
                SecretCommand::Set {
                    name,
                    files,
                    state_dir,
                },
        } => set_secret(&name, &files, &state_dir),
        Command::Compile { workflow, out } => orchestrate(
            &workflow,
            Mode::Compile,
            &How {
                out: out.as_deref(),
                ..How::default()
            },
        ),
        Command::Plan {
            workflow,
            detailed_exitcode,
        } => orchestrate(
            &workflow,
            Mode::Plan,
            &How {
                detailed_exitcode,
                ..How::default()
            },
        ),
        Command::Apply {
            workflow,
            resume,
            on_failure,
        } => orchestrate(
            &workflow,
            Mode::Apply,
            &How {
                resume: resume.as_deref(),
                rollback_on_failure: on_failure == FailureAction::Rollback,
                ..How::default()
            },
        ),
        Command::Drift { workflow } => orchestrate(&workflow, Mode::Drift, &How::default()),
        Command::Rollback { workflow, run } => orchestrate(
            &workflow,
            Mode::Rollback,
            &How {
                rollback_of: run.as_deref(),
                ..How::default()
            },
        ),
        Command::Status { state } => status(&state),
        Command::Runs { state } => runs(&state),
        Command::Events { run, types, state } => events(&state, run.as_deref(), &types),
        Command::Report {
            run,
            format,
            out,
            from,
            state,
        } => report(
            &state,
            run.as_deref(),
            format,
            out.as_deref(),
            from.as_deref(),
        ),
        Command::Unlock { id, state } => unlock(&state, &id),
        Command::Import {
            paths,
            from,
            name,
            out,
            recursive,
            force,
            strict,
            plugins,
        } => import_configuration(
            &plugins,
            &import::Request {
                paths: &paths,
                from: from.as_deref(),
                name: name.as_deref(),
                out: out.as_deref(),
                recursive,
                force,
                strict,
            },
        ),
        Command::Plugins { settings, plugins } => list_plugins(&plugins, settings),
        Command::Rules {
            command:
                RulesCommand::Check {
                    hierarchy,
                    rules,
                    require_rules,
                    set,
                    data,
                    state_dir,
                },
        } => check_rules(
            meridian_rules::Files {
                hierarchy,
                rules,
                require_rules,
            },
            &set,
            &data,
            &state_dir,
        ),
        Command::Migrate { files, to, check } => migrate(&files, to.as_deref(), check),
        Command::Version => {
            version();
            Ok(())
        }
        Command::Fmt(args) => tools::format(&args),
        Command::Lint(args) => tools::lint(&args),
        Command::Schema(args) => tools::schema(&args),
        Command::Lsp(args) => tools::lsp(&args),
        Command::Signing { command } => signing(command),
    });
    // What stdout holds is the command's result, so a result that could not
    // be written (other than to a reader that went away) is a failure.
    let code: u8 = match outcome {
        Ok(()) => 0,
        Err(Failure::Reported) => 1,
        Err(Failure::Drifted) | Err(Failure::Changes) => 2,
        // The status a shell gives a command ended by Ctrl-C.
        Err(Failure::Interrupted) => 130,
        Err(Failure::Message(msg)) => {
            eprintln!("error: {msg}");
            // The machine reading stdout is told as well: an empty stdout
            // and a status would make it read stderr after all.
            if json_output() {
                emit_error(&msg);
            }
            1
        }
    };
    let _ = Stdout.flush();
    if code == 0 && Stdout::failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::from(code)
    }
}

pub(crate) enum Failure {
    /// Something to print.
    Message(String),
    /// Already printed, stage by stage.
    Reported,
    /// A drift check ran, and found drift. Already printed.
    Drifted,
    /// The operator stopped the run. Already printed.
    Interrupted,
    /// `plan --detailed-exitcode` found something an apply would do. Already
    /// printed.
    Changes,
}
