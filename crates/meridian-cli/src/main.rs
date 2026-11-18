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
mod pipeline;
mod print;
mod schema_bundles;
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
    describe_duration, file_hash, lock, record, run_selected, selected, Asked, Backend, Decisions,
    Drift, Event, ExecBackend, Keeper, LocalBackend, LockInfo, Mode, Observer, OnFailure, Options,
    Outcome, PlanFile, Policy, Reason, Refusal, Resume, RollbackOutcome, RollbackReport, RunRecord,
    RunReport, RunState, Selector, StageId, StageReport, Stop, MASK,
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
use pipeline::*;
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
    /// Inside a pipeline engine (GitHub Actions, Azure Pipelines, GitLab CI,
    /// Buildkite, Jenkins, Argo Workflows, Tekton), also write each problem
    /// in the engine's own annotation syntax and the run's report as the
    /// job's summary: auto does so when standard output is not a terminal,
    /// on does so regardless, off never does
    #[arg(
        long,
        global = true,
        value_name = "MODE",
        env = "MERIDIAN_ANNOTATIONS",
        default_value = "auto"
    )]
    annotations: AnnotationMode,
}

/// The stack the command runs on. Windows gives the main thread 1 MiB, which
/// an unoptimised build of `run`, holding every command's locals in one
/// frame, outgrew; the stack is set here rather than by each platform.
const STACK: usize = 16 * 1024 * 1024;

fn main() -> ExitCode {
    match std::thread::Builder::new()
        .name("meridian".to_owned())
        .stack_size(STACK)
        .spawn(run)
    {
        Ok(command) => command
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
        Err(e) => {
            eprintln!("error: starting the command: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> ExitCode {
    // Before anything is read: no core dump of this process, and no other
    // process of the same user reading its memory, so a secret fetched later
    // cannot leave that way.
    let _ = meridian_secret::harden_process();
    let cli = Cli::parse();
    settle_output(cli.output, &cli.command.name());
    settle_pipeline(cli.annotations);
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
            out,
        } => orchestrate(
            &workflow,
            Mode::Plan,
            &How {
                detailed_exitcode,
                plan_out: out.as_deref(),
                ..How::default()
            },
        ),
        Command::Apply {
            workflow,
            resume,
            on_failure,
            plan,
        } => orchestrate(
            &workflow,
            Mode::Apply,
            &How {
                resume: resume.as_deref(),
                rollback_on_failure: on_failure == FailureAction::Rollback,
                from_plan: plan.as_deref(),
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
        Command::Logs {
            run,
            step,
            endpoint,
            stream,
            grep,
            tail,
            all,
            state,
        } => show_logs(
            &state,
            &LogQuery {
                run: run.as_deref(),
                step: step.as_deref(),
                endpoint: endpoint.as_deref(),
                stream,
                grep: grep.as_deref(),
                tail,
                all,
            },
        ),
        Command::BugReport { run, out, state } => {
            bug_report(&state, run.as_deref(), out.as_deref())
        }
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
        Command::Plugins {
            settings,
            rules,
            plugins,
        } => list_plugins(&plugins, settings, rules),
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
        Command::Retarget {
            file,
            to,
            out,
            no_adopt,
            state_dir,
            plugins,
        } => retarget(&file, &to, out.as_deref(), no_adopt, &state_dir, &plugins),
        Command::Version => {
            version();
            Ok(())
        }
        Command::Test {
            files,
            case,
            junit,
            plugins,
            state_dir,
        } => test(
            &files,
            case.as_deref(),
            junit.as_deref(),
            &plugins,
            &state_dir,
        ),
        Command::Fmt(args) => tools::format(&args),
        Command::Lint(args) => tools::lint(&args),
        Command::Schema(args) => tools::schema(&args),
        Command::Lsp(args) => tools::lsp(&args),
        Command::Mcp(args) => tools::mcp(&args),
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
        Err(Failure::Usage(msg)) => {
            eprintln!("error: {msg}");
            if json_output() {
                emit_error(&msg);
            }
            2
        }
        Err(Failure::Message(msg)) => {
            eprintln!("error: {msg}");
            // The machine reading stdout is told as well: an empty stdout
            // and a status would make it read stderr after all.
            if json_output() {
                emit_error(&msg);
            }
            // And the pipeline engine, where there is one, so the problem
            // shows on the job and not only in its log.
            report_refusal(&msg);
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
    /// The command line does not make sense, as `clap` would have said:
    /// exit 2, so that a script tells a wrong command from a failed one.
    Usage(String),
}
