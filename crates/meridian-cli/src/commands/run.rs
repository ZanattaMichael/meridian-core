//! The commands that run or read a run: `compile`, `plan`, `apply`, `drift`,
//! `rollback`, `status`, `runs`, `events`, `report` and `unlock`.

use crate::*;

/// What a command adds to the workflow it runs.
#[derive(Default)]
pub(crate) struct How<'a> {
    /// `compile --out`
    pub(crate) out: Option<&'a Path>,
    /// `apply --resume`
    pub(crate) resume: Option<&'a str>,
    /// `apply --on-failure rollback`
    pub(crate) rollback_on_failure: bool,
    /// `rollback --run`
    pub(crate) rollback_of: Option<&'a str>,
    /// `plan --detailed-exitcode`
    pub(crate) detailed_exitcode: bool,
    /// `plan --out`
    pub(crate) plan_out: Option<&'a Path>,
    /// `apply --plan`
    pub(crate) from_plan: Option<&'a Path>,
    /// `plan --destroy`
    pub(crate) destroy: bool,
    /// `destroy --yes`
    pub(crate) confirmed: bool,
}

/// What `apply --plan` is given, against what the plan was taken with: a
/// thing left out on the command line is the plan's, and one given must be
/// the plan's, so the apply is of the plan and not of something like it.
fn from_the_plan<T: Clone + PartialEq>(
    what: &str,
    given: &[T],
    planned: &[T],
    show: impl Fn(&T) -> String,
) -> Result<Vec<T>, Failure> {
    if given.is_empty() {
        return Ok(planned.to_vec());
    }
    if given != planned {
        let list = |items: &[T]| items.iter().map(&show).collect::<Vec<_>>().join(" ");
        return Err(Failure::Message(format!(
            "the plan was taken with {what} `{}`, and this apply names `{}`; give the plan's, \
             or leave {what} out to take it from the plan",
            list(planned),
            list(given)
        )));
    }
    Ok(given.to_vec())
}

pub(crate) fn orchestrate(workflow: &Workflow, mode: Mode, how: &How<'_>) -> Result<(), Failure> {
    let (out, resume) = (how.out, how.resume);
    if workflow.events.is_some() && json_output() {
        // Both want standard output for themselves.
        return Err(Failure::Message(
            "--events json writes every event to standard output, where --output json writes \
             the result; use one of them, or --report json=FILE for the run as a file"
                .to_owned(),
        ));
    }
    // What is run: the command line's, or with `apply --plan` the plan's
    // where the command line left it out.
    let mut plan_file = match how.from_plan {
        None => None,
        Some(path) => Some(PlanFile::read(path).map_err(Failure::Message)?),
    };
    // Which data files are not what the plan was taken with. This never
    // refuses the apply: it only names the file when a stage then fails
    // with `plan-differs`.
    if let Some(plan) = plan_file.as_mut() {
        plan.changed_data = plan
            .asked
            .data
            .iter()
            .zip(&plan.asked.data_hashes)
            .filter(|(path, hash)| {
                std::fs::read(path).map_or(true, |bytes| file_hash(&bytes) != **hash)
            })
            .map(|(path, _)| path.clone())
            .collect();
    }
    let planned = plan_file.as_ref().map(|plan| &plan.asked);
    let path_text = |path: &PathBuf| path.display().to_string();
    let (files, data_files, sets, selectors) = match planned {
        None => (
            workflow.files.clone(),
            workflow.data.clone(),
            workflow.set.clone(),
            workflow.selector.clone(),
        ),
        Some(asked) => (
            from_the_plan(
                "the documents",
                &workflow.files,
                &asked.files.iter().map(PathBuf::from).collect::<Vec<_>>(),
                path_text,
            )?,
            from_the_plan(
                "--data",
                &workflow.data,
                &asked.data.iter().map(PathBuf::from).collect::<Vec<_>>(),
                path_text,
            )?,
            from_the_plan("--set", &workflow.set, &asked.set, String::clone)?,
            if workflow.selector.is_empty() && !asked.selector.is_empty() {
                vec![asked.selector.clone()]
            } else {
                workflow.selector.clone()
            },
        ),
    };
    if files.is_empty() {
        return Err(Failure::Usage(format!(
            "the following required arguments were not provided:\n  <FILES>...\n\nUsage: \
             meridian {} <FILES>...{}",
            mode.as_str(),
            if mode == Mode::Apply {
                "\n\nWith `apply --plan FILE`, the documents may be left out and are taken from \
                 the plan"
            } else {
                ""
            }
        )));
    }
    let bundle = load(&files, &workflow.state.state_dir)?;
    let mut selector = Selector::all();
    for text in &selectors {
        selector = selector.and(Selector::parse(text).map_err(Failure::Message)?);
    }
    if let Some(asked) = planned {
        if selector.to_string() != asked.selector {
            return Err(Failure::Message(format!(
                "the plan was taken with the selector `{}`, and this apply selects `{selector}`; \
                 give the plan's, or leave -l out to take it from the plan",
                asked.selector
            )));
        }
    }
    let mut data = read_data(&data_files, &workflow.state.state_dir)?;
    apply_sets(&mut data, &sets)?;
    if selected(&bundle, &selector, &data) == 0 {
        // Doing nothing and reporting success would read as "all applied".
        return Err(Failure::Message(format!(
            "no document's labels match the selector {selector}"
        )));
    }
    // Where the reports go and who is sent the events are settled before
    // anything runs, so a misspelt format cannot cost a finished apply its
    // report.
    let reports = workflow
        .report
        .iter()
        .map(|text| parse_report(text))
        .collect::<Result<Vec<_>, _>>()
        .map_err(Failure::Message)?;
    let sinks = sink::specs(
        &workflow.sink,
        &workflow.sink_config,
        &plugin_dirs(&workflow.state.plugins),
    )
    .map_err(Failure::Message)?;
    // What was typed, for the record: the files and the selection. Backend
    // settings are left out, because one of them can be a credential.
    let mut command = vec![mode.as_str().to_owned()];
    command.extend(files.iter().map(|f| f.display().to_string()));
    for text in &selectors {
        command.push(format!("-l {text}"));
    }
    if let Some(path) = how.plan_out {
        command.push(format!("--out {}", path.display()));
    }
    if let Some(path) = how.from_plan {
        command.push(format!("--plan {}", path.display()));
    }
    match resume {
        None => {}
        Some("last") => command.push("--resume".to_owned()),
        Some(run) => command.push(format!("--resume={run}")),
    }
    if how.rollback_on_failure {
        command.push("--on-failure rollback".to_owned());
    }
    if workflow.tool_debug > 0 {
        command.push(format!("--tool-debug={}", workflow.tool_debug));
        for step in &workflow.tool_debug_step {
            command.push(format!("--tool-debug-step {step}"));
        }
    }
    if how.destroy {
        command.push("--destroy".to_owned());
    }
    if let Some(limit) = workflow.stage_timeout {
        command.push(format!("--stage-timeout {}", describe_duration(limit)));
    }
    for path in &workflow.policy {
        command.push(format!("--policy {}", path.display()));
    }
    let policy = if workflow.policy.is_empty() {
        None
    } else if !matches!(mode, Mode::Plan | Mode::Apply) || how.destroy {
        // Saying nothing would read as "checked".
        return Err(Failure::Message(format!(
            "--policy is checked by plan and apply, and {} asks no policy; remove it",
            if how.destroy {
                "plan --destroy"
            } else {
                mode.as_str()
            }
        )));
    } else {
        Some(
            Policy::find(
                &workflow.policy_engine,
                &plugin_dirs(&workflow.state.plugins),
                workflow.policy.clone(),
            )
            .map_err(Failure::Message)?,
        )
    };
    let options = Options {
        data,
        // Compiling keeps no record, so it needs no backend to be there.
        backend: match mode {
            Mode::Compile => None,
            Mode::Plan | Mode::Apply | Mode::Drift | Mode::Rollback | Mode::Destroy => {
                Some(backend(&workflow.state)?)
            }
        },
        lock_timeout: workflow.lock_timeout,
        resume: resume.map(|run| match run {
            "last" => Resume::Last,
            id => Resume::Run(id.to_owned()),
        }),
        command: command.join(" "),
        keep_runs: workflow.keep_runs,
        on_failure: if how.rollback_on_failure {
            OnFailure::Rollback
        } else {
            OnFailure::Stop
        },
        rollback_of: match how.rollback_of {
            None => Resume::Last,
            Some(id) => Resume::Run(id.to_owned()),
        },
        policy,
        save_plan: how.plan_out.is_some(),
        from_plan: plan_file.as_ref().map(|plan| Arc::new(plan.clone())),
        destroy: how.destroy,
        strict_secrets: workflow.strict_secrets,
        tool_debug: workflow.tool_debug,
        tool_debug_steps: workflow.tool_debug_step.clone(),
        // Compiling drives no tool, so there is nothing to interrupt but the
        // process itself, which Ctrl-C does as it always did.
        stop: Stop {
            when: (mode != Mode::Compile).then(interrupt::install),
            after: workflow.stage_timeout,
            ..Stop::default()
        },
        ..Options::new(&workflow.state.state_dir)
    };
    if mode == Mode::Destroy && !how.confirmed {
        confirm_destroy(&bundle, &selector, &options.data, &files)?;
    }
    let registry = registry(&workflow.state.plugins)?;
    let mut printer = Printer {
        mode,
        out,
        events: workflow.events.is_some(),
        json: json_output(),
        seen: Vec::new(),
        sinks: Dispatcher::start(sinks, workflow.sink_timeout).map_err(Failure::Message)?,
    };
    // Each step's tool output is kept, masked, on this machine; a compile
    // runs no tool and keeps no record.
    let (report, log_warnings) = if mode == Mode::Compile || workflow.no_logs {
        let report = run_selected(&bundle, &registry, mode, &options, &selector, &mut printer);
        (report, Vec::new())
    } else {
        let mut keeper = Keeper::new(&mut printer, &workflow.state.state_dir, workflow.log_limit);
        let report = run_selected(&bundle, &registry, mode, &options, &selector, &mut keeper);
        let warnings = keeper.close();
        (report, warnings)
    };
    for warning in &log_warnings {
        eprintln!("warning: {warning}");
    }
    // The sinks are closed and the reports written whatever the run did: a
    // failed run is the one most worth hearing about.
    let mut sink_failed = false;
    for delivery in printer.sinks.finish() {
        if let Some(problem) = delivery.problem() {
            if delivery.required {
                sink_failed = true;
                eprintln!("error: {problem}");
            } else {
                eprintln!("warning: {problem}");
            }
        }
    }
    if !printer.seen.is_empty() {
        let mut folded = Report::from_events(&printer.seen);
        if let Some(backend) = &options.backend {
            if let Ok(Some(record)) = record::load_run(backend.as_ref(), &report.run_id) {
                folded = folded.with_record(&record);
            }
        }
        for (format, path) in &reports {
            match write_report(&folded, *format, path) {
                Ok(()) => eprintln!("report: wrote {} to {}", format.as_str(), path.display()),
                Err(why) => eprintln!("warning: {why}"),
            }
        }
        // The pipeline engine, where there is one, is shown the same report
        // as its job summary, and each failed step as an annotation at its
        // document.
        if engine().is_some() {
            annotate(&meridian_report::engine::problems(
                &folded,
                &positions(&bundle),
            ));
            let kept_in = options
                .backend
                .as_ref()
                .map(|backend| backend.describe())
                .unwrap_or_default();
            summarise(&meridian_report::engine::run_summary(&folded, &kept_in));
        }
    }
    // A plugin that will not exit is worth a warning, not a failed run.
    if let Err(e) = registry.close() {
        eprintln!("warning: {e}");
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    // The plan is written once the run is over and every stage planned; a
    // plan with a failed stage is nothing anyone can apply, and saying so
    // beats a file that looks like one.
    let mut plan_about = None;
    let mut plan_failed = false;
    if let Some(path) = how.plan_out {
        if report.refused.is_none() {
            let written = options
                .backend
                .as_ref()
                .ok_or_else(|| "a plan keeps a record".to_owned())
                .and_then(|backend| RunState::read(backend.as_ref()))
                .and_then(|state| {
                    PlanFile::from_report(
                        &report,
                        &state,
                        Asked {
                            files: files.iter().map(|f| f.display().to_string()).collect(),
                            data: data_files.iter().map(|f| f.display().to_string()).collect(),
                            data_hashes: data_files
                                .iter()
                                .map(|f| {
                                    std::fs::read(f).map(|b| file_hash(&b)).unwrap_or_default()
                                })
                                .collect(),
                            set: sets.clone(),
                            selector: selector.to_string(),
                            command: options.command.clone(),
                        },
                    )
                })
                .and_then(|plan| plan.write(path).map(|()| plan));
            match written {
                Ok(plan) => {
                    eprintln!("plan: wrote {}", path.display());
                    plan_about = Some(PlanAbout {
                        file: path.to_path_buf(),
                        run: String::new(),
                        has_changes: plan.has_changes(),
                    });
                }
                Err(why) => {
                    eprintln!("error: the plan was not saved: {why}");
                    plan_failed = true;
                }
            }
        }
    }
    if let Some(plan) = &plan_file {
        plan_about = Some(PlanAbout {
            file: how.from_plan.map(Path::to_path_buf).unwrap_or_default(),
            run: plan.run.clone(),
            has_changes: plan.has_changes(),
        });
    }
    // The one object a machine reads, whatever the run did; the lines that
    // follow are for the person reading standard error.
    if json_output() {
        let ok = report.refused.is_none() && report.succeeded() && !sink_failed && !plan_failed;
        emit(ok, output::run(&report, mode, out, plan_about.as_ref()));
    }
    if let Some(refusal) = &report.refused {
        if json_output() {
            eprintln!("error: {refusal}");
            report_refusal(&refusal.to_string());
            return Err(Failure::Reported);
        }
        return Err(Failure::Message(refusal.to_string()));
    }
    if mode == Mode::Drift {
        let drifted = report
            .stages
            .iter()
            .filter(|s| matches!(s.outcome, Outcome::Drift(Drift::Drifted(_))))
            .count();
        let line = format!(
            "drift: {drifted} of {} drifted",
            count(report.stages.len(), "stage")
        );
        if workflow.events.is_some() || json_output() {
            eprintln!("{line}");
        } else {
            outln!("{line}");
        }
    }
    if !report.succeeded() {
        let failed: Vec<&StageReport> = report
            .stages
            .iter()
            .filter(|s| matches!(s.outcome, Outcome::Failed(_)))
            .collect();
        let denied = !failed.is_empty()
            && failed
                .iter()
                .all(|s| s.reason == Some(Reason::PolicyDenied));
        let applied = report
            .stages
            .iter()
            .any(|s| matches!(s.outcome, Outcome::Applied { .. }));
        // A line that can be pasted: the same documents and selection.
        let files: Vec<String> = workflow
            .files
            .iter()
            .map(|f| f.display().to_string())
            .collect();
        let selectors: Vec<String> = workflow
            .selector
            .iter()
            .map(|text| format!(" -l {text}"))
            .collect();
        if report.interrupted() {
            if mode == Mode::Apply {
                eprintln!(
                    "run {} was interrupted. `meridian apply --resume {}{}` continues from the \
                     stage that was stopped",
                    report.run_id,
                    files.join(" "),
                    selectors.join("")
                );
            } else {
                eprintln!("run {} was interrupted", report.run_id);
            }
            return Err(Failure::Interrupted);
        }
        if denied && mode == Mode::Apply && !applied {
            eprintln!(
                "run {} was stopped by its policy before anything was applied",
                report.run_id
            );
        } else if mode == Mode::Apply && how.from_plan.is_some() {
            eprintln!(
                "run {} failed. A saved plan is applied whole: take a new plan with \
                 `meridian plan --out` and apply that",
                report.run_id
            );
        } else if mode == Mode::Apply && report.rollbacks.is_empty() {
            eprintln!(
                "run {} failed. Once the cause is fixed, `meridian apply --resume {}{}` \
                 continues from the stage that failed",
                report.run_id,
                files.join(" "),
                selectors.join("")
            );
        }
        return Err(Failure::Reported);
    }
    if report.drifted() {
        return Err(Failure::Drifted);
    }
    if sink_failed {
        return Err(Failure::Reported);
    }
    if plan_failed {
        return Err(Failure::Reported);
    }
    if how.detailed_exitcode && report.stages.iter().any(stage_has_changes) {
        return Err(Failure::Changes);
    }
    Ok(())
}

/// Asks the person at the terminal before anything is destroyed. Without a
/// terminal to ask, `--yes` is the only answer.
fn confirm_destroy(
    bundle: &meridian_ir::Bundle,
    selector: &Selector,
    data: &meridian_sdk::Data,
    files: &[PathBuf],
) -> Result<(), Failure> {
    use std::io::{BufRead, IsTerminal, Write};
    let again = || {
        let files: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
        files.join(" ")
    };
    if json_output() || !std::io::stdin().is_terminal() {
        return Err(Failure::Message(format!(
            "destroy asks before it removes anything, and there is no terminal to ask; \
             `meridian plan --destroy {0}` shows what would go, and `meridian destroy --yes \
             {0}` destroys it",
            again()
        )));
    }
    let count = selected(bundle, selector, data);
    eprint!(
        "This removes what the tools of {} made, last first. `meridian plan --destroy {}` \
         shows what would go.\nType yes to destroy: ",
        count_of(count, "document"),
        again()
    );
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|e| Failure::Message(format!("reading the answer: {e}")))?;
    if answer.trim() != "yes" {
        return Err(Failure::Message("nothing was destroyed".to_owned()));
    }
    Ok(())
}

fn count_of(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

/// Writes a report to a file, making the directory it goes in.
pub(crate) fn write_report(report: &Report, format: Format, path: &Path) -> Result<(), String> {
    let failed = |e: std::io::Error| {
        format!(
            "the {} report could not be written to {}: {e}",
            format.as_str(),
            path.display()
        )
    };
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    std::fs::write(path, meridian_report::render(report, format)).map_err(failed)
}

/// Reports on a recorded run, or on a file of events.
pub(crate) fn report(
    state: &State,
    run: Option<&str>,
    format: Format,
    out: Option<&Path>,
    from: Option<&Path>,
) -> Result<(), Failure> {
    let folded = match from {
        Some(path) => {
            let text = if path == Path::new("-") {
                std::io::read_to_string(std::io::stdin())
            } else {
                std::fs::read_to_string(path)
            }
            .map_err(|e| Failure::Message(format!("reading {}: {e}", path.display())))?;
            let events = meridian_orchestrator::event::from_jsonl(&text);
            if events.is_empty() {
                return Err(Failure::Message(format!(
                    "{} holds no events; it should be what `--events json` wrote",
                    path.display()
                )));
            }
            Report::from_events(&events)
        }
        None => {
            let backend = backend(state)?;
            let backend = backend.as_ref();
            let record = match run {
                Some(id) => record::load_run(backend, id)
                    .map_err(Failure::Message)?
                    .ok_or_else(|| {
                        Failure::Message(format!("there is no run {id} in {}", backend.describe()))
                    })?,
                None => record::last_run(backend, None)
                    .map_err(Failure::Message)?
                    .ok_or_else(|| {
                        Failure::Message(format!(
                            "no run has been recorded in {}",
                            backend.describe()
                        ))
                    })?,
            };
            let events = record::load_events(backend, &record.id).map_err(Failure::Message)?;
            Report::from_events(&events).with_record(&record)
        }
    };
    match out {
        Some(path) => write_report(&folded, format, path).map_err(Failure::Message),
        None => {
            let _ = std::io::stdout()
                .lock()
                .write_all(meridian_report::render(&folded, format).as_bytes());
            Ok(())
        }
    }
}

/// Shows the current or last run, and whether the state is locked.
pub(crate) fn status(state: &State) -> Result<(), Failure> {
    let backend = backend(state)?;
    let backend = backend.as_ref();
    let held = lock::holder(backend).map_err(Failure::Message)?;
    let last = record::last_run(backend, None).map_err(Failure::Message)?;
    if json_output() {
        emit(
            true,
            body([
                ("backend", backend.describe().into()),
                (
                    "run",
                    last.as_ref().map_or(serde_json::Value::Null, |run| {
                        output::record(run, held.as_ref().is_some_and(|lock| lock.id == run.id))
                    }),
                ),
                (
                    "lock",
                    held.as_ref().map_or(serde_json::Value::Null, output::lock),
                ),
            ]),
        );
        return Ok(());
    }
    match last {
        None => outln!("no run has been recorded in {}", backend.describe()),
        Some(run) => print_run(&run, held.as_ref().is_some_and(|lock| lock.id == run.id)),
    }
    match held {
        Some(lock) => outln!("locked: {lock}"),
        None => outln!("not locked"),
    }
    Ok(())
}

/// Lists every recorded run, oldest first.
pub(crate) fn runs(state: &State) -> Result<(), Failure> {
    let backend = backend(state)?;
    let backend = backend.as_ref();
    let ids = record::list_runs(backend).map_err(Failure::Message)?;
    if json_output() {
        let held = lock::holder(backend).map_err(Failure::Message)?;
        let mut runs = Vec::new();
        for id in ids {
            if let Some(run) = record::load_run(backend, &id).map_err(Failure::Message)? {
                let holds = held.as_ref().is_some_and(|lock| lock.id == run.id);
                runs.push(output::record(&run, holds));
            }
        }
        emit(
            true,
            body([
                ("backend", backend.describe().into()),
                ("runs", serde_json::Value::Array(runs)),
            ]),
        );
        return Ok(());
    }
    if ids.is_empty() {
        outln!("no run has been recorded in {}", backend.describe());
        return Ok(());
    }
    for id in ids {
        let Some(run) = record::load_run(backend, &id).map_err(Failure::Message)? else {
            continue;
        };
        let failed = run
            .steps
            .iter()
            .filter(|step| step.status == record::StepStatus::Failed)
            .count();
        let mut line = format!(
            "{}  {:<8} {:<10} {}",
            run.id,
            run.mode,
            run.status.as_str(),
            count(run.steps.len(), "step")
        );
        if failed > 0 {
            line.push_str(&format!(", {failed} failed"));
        }
        if !run.resumes.is_empty() {
            line.push_str(&format!(", resumes {}", run.resumes));
        }
        outln!("{line}");
    }
    Ok(())
}

/// Prints a run's events as they were recorded.
pub(crate) fn events(state: &State, run: Option<&str>, types: &[String]) -> Result<(), Failure> {
    let backend = backend(state)?;
    let backend = backend.as_ref();
    let id = match run {
        Some(id) => {
            if record::load_run(backend, id)
                .map_err(Failure::Message)?
                .is_none()
            {
                return Err(Failure::Message(format!(
                    "there is no run {id} in {}",
                    backend.describe()
                )));
            }
            id.to_owned()
        }
        None => match record::last_run(backend, None).map_err(Failure::Message)? {
            Some(run) => run.id,
            None => {
                return Err(Failure::Message(format!(
                    "no run has been recorded in {}",
                    backend.describe()
                )))
            }
        },
    };
    let mut out = Stdout;
    for event in record::load_events(backend, &id).map_err(Failure::Message)? {
        if types.is_empty() || types.iter().any(|pattern| event.matches(pattern)) {
            let _ = writeln!(out, "{}", event.to_json());
        }
    }
    Ok(())
}

/// Removes a lock a run left behind.
pub(crate) fn unlock(state: &State, id: &str) -> Result<(), Failure> {
    let backend = backend(state)?;
    let removed = lock::force_unlock(backend.as_ref(), id).map_err(Failure::Message)?;
    if json_output() {
        emit(true, body([("removed", output::lock(&removed))]));
    } else {
        outln!("removed {removed}");
    }
    Ok(())
}
