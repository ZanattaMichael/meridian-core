//! Sinks: programs that are sent a run's events as they happen.
//!
//! A sink is how a run reaches somewhere else: a webhook, a chat channel, a
//! collector, a report server. Meridian does not speak any of those itself.
//! It starts a program, writes each event to the program's standard input as
//! one line of JSON, and closes the input when the run is over. The program
//! does the sending, and exits 0 when everything it was given was delivered.
//!
//! | Sink | The program |
//! |---|---|
//! | `command` | the one named by its `program` setting |
//! | any other name | `meridian-notify-<name>`, found in a plugin directory |
//!
//! A setting reaches the program as `MERIDIAN_NOTIFY_<SETTING>`. Two settings
//! are Meridian's own and are not passed on: `on`, the event types the sink
//! is sent (`run.*,step.failed`; everything by default), and `required`,
//! whether a sink that fails also fails the run.
//!
//! A sink never holds a step up. Each one is fed from its own queue by its
//! own thread, and an event that does not fit in the queue is dropped and
//! counted, not waited for. What a sink is sent is an event, and an event
//! holds no value a document or a tool produced, so nothing can leak
//! through one.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use meridian_orchestrator::Event;

/// What a sink program's file name starts with.
pub const SINK_PREFIX: &str = "meridian-notify-";
/// The version of the contract between Meridian and a sink program, which
/// the program reads as `MERIDIAN_NOTIFY_PROTOCOL`.
pub const PROTOCOL: &str = "1";
/// How many events wait for a slow sink before more are dropped.
pub const QUEUE: usize = 4096;
/// How long a sink has, after the run's last event, to finish sending.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(30);

/// One sink, as it was asked for.
#[derive(Clone, PartialEq, Eq)]
pub struct SinkSpec {
    /// The name it was asked for by.
    pub name: String,
    pub program: PathBuf,
    /// The sink's own settings.
    pub settings: BTreeMap<String, String>,
    /// The event types it is sent; `*` and `step.*` are accepted.
    pub on: Vec<String>,
    /// Whether the run fails when the sink does.
    pub required: bool,
}

/// A setting's value can be a credential, so `{:?}` shows only its name.
impl fmt::Debug for SinkSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SinkSpec")
            .field("name", &self.name)
            .field("program", &self.program)
            .field("settings", &self.settings.keys().collect::<Vec<_>>())
            .field("on", &self.on)
            .field("required", &self.required)
            .finish()
    }
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl SinkSpec {
    /// Works out the sink named `name` from its settings: finds its program
    /// in `dirs`, the directories plugins are looked for in, and takes
    /// Meridian's own settings out of the ones the program is given.
    pub fn find(
        name: &str,
        dirs: &[PathBuf],
        mut settings: BTreeMap<String, String>,
    ) -> Result<SinkSpec, String> {
        if !safe_name(name) {
            return Err(format!("{name:?} is not a sink name"));
        }
        let on = match settings.remove("on") {
            None => vec!["*".to_owned()],
            Some(list) => {
                let on: Vec<String> = list
                    .split(',')
                    .map(|pattern| pattern.trim().to_owned())
                    .filter(|pattern| !pattern.is_empty())
                    .collect();
                if let Some(bad) = on.iter().find(|pattern| !pattern_ok(pattern)) {
                    return Err(format!(
                        "sink {name:?}: {bad:?} is not an event type; give a type such as \
                         step.failed, a family such as step.*, or *"
                    ));
                }
                if on.is_empty() {
                    return Err(format!("sink {name:?}: `on` names no event type"));
                }
                on
            }
        };
        let required = match settings.remove("required").as_deref() {
            None | Some("false") => false,
            Some("true") => true,
            Some(other) => {
                return Err(format!(
                    "sink {name:?}: required is true or false, not {other:?}"
                ))
            }
        };
        let program = if name == "command" {
            let program = settings.remove("program").ok_or_else(|| {
                "the command sink needs the program to run: --sink-config command.program=PATH"
                    .to_owned()
            })?;
            PathBuf::from(program)
        } else {
            let file = format!("{SINK_PREFIX}{name}");
            dirs.iter()
                .flat_map(|dir| [dir.join(&file), dir.join(format!("{file}.exe"))])
                .find(|candidate| candidate.is_file())
                .ok_or_else(|| {
                    let searched: Vec<String> =
                        dirs.iter().map(|d| d.display().to_string()).collect();
                    format!(
                        "no sink {name:?}: there is no program named {file} in {}",
                        if searched.is_empty() {
                            "any plugin directory".to_owned()
                        } else {
                            searched.join(", ")
                        }
                    )
                })?
        };
        Ok(SinkSpec {
            name: name.to_owned(),
            program,
            settings,
            on,
            required,
        })
    }

    /// Whether the sink is sent `event`.
    pub fn wants(&self, event: &Event) -> bool {
        self.on.iter().any(|pattern| event.matches(pattern))
    }
}

fn pattern_ok(pattern: &str) -> bool {
    let body = pattern.strip_suffix(".*").unwrap_or(pattern);
    pattern == "*"
        || (!body.is_empty()
            && body
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'))
}

/// The environment variable a setting arrives in.
pub fn setting_variable(setting: &str) -> String {
    let name: String = setting
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("MERIDIAN_NOTIFY_{name}")
}

/// How one sink did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub name: String,
    pub required: bool,
    /// Events the sink's program was given.
    pub sent: u64,
    /// Events that were not given to it: its queue was full, or it had
    /// already failed.
    pub dropped: u64,
    /// Why it failed, when it did.
    pub error: Option<String>,
}

impl Delivery {
    /// Whether everything the sink was meant to get, it got.
    pub fn delivered(&self) -> bool {
        self.error.is_none() && self.dropped == 0
    }

    /// What went wrong, in a sentence; `None` when nothing did.
    pub fn problem(&self) -> Option<String> {
        match (&self.error, self.dropped) {
            (None, 0) => None,
            (Some(why), _) => Some(format!("sink {:?} failed: {why}", self.name)),
            (None, n) => Some(format!(
                "sink {:?} was too slow: {n} events were not sent to it",
                self.name
            )),
        }
    }
}

struct Running {
    spec: SinkSpec,
    queue: Option<SyncSender<String>>,
    worker: JoinHandle<(u64, Option<String>)>,
    dropped: u64,
}

/// Sends a run's events to its sinks.
pub struct Dispatcher {
    sinks: Vec<Running>,
}

impl Dispatcher {
    /// Starts every sink's program. A program that cannot be started is an
    /// error here, before the run has done anything. `grace` is how long
    /// each has, after the run's last event, to finish sending.
    pub fn start(specs: Vec<SinkSpec>, grace: Duration) -> Result<Dispatcher, String> {
        let mut sinks = Vec::new();
        for spec in specs {
            let mut command = Command::new(&spec.program);
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .env("MERIDIAN_NOTIFY_PROTOCOL", PROTOCOL)
                .env("MERIDIAN_NOTIFY_NAME", &spec.name);
            for (setting, value) in &spec.settings {
                command.env(setting_variable(setting), value);
            }
            let child = command.spawn().map_err(|e| {
                format!(
                    "sink {:?}: {} could not be started: {e}",
                    spec.name,
                    spec.program.display()
                )
            })?;
            let (queue, events) = sync_channel::<String>(QUEUE);
            let worker = thread::spawn(move || feed(child, &events, grace));
            sinks.push(Running {
                spec,
                queue: Some(queue),
                worker,
                dropped: 0,
            });
        }
        Ok(Dispatcher { sinks })
    }

    /// Whether there is any sink at all.
    pub fn is_empty(&self) -> bool {
        self.sinks.is_empty()
    }

    /// Hands `event` to every sink that asked for its type. It never waits:
    /// a sink whose queue is full misses the event.
    pub fn send(&mut self, event: &Event) {
        let mut line: Option<String> = None;
        for sink in &mut self.sinks {
            if !sink.spec.wants(event) {
                continue;
            }
            let Some(queue) = &sink.queue else { continue };
            let line = line.get_or_insert_with(|| event.to_json());
            match queue.try_send(line.clone()) {
                Ok(()) => {}
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => sink.dropped += 1,
            }
        }
    }

    /// Closes every sink's input, waits for each to finish, and says how
    /// each did. A sink that is still going after the grace period is
    /// stopped and counts as failed.
    pub fn finish(self) -> Vec<Delivery> {
        self.sinks
            .into_iter()
            .map(|mut sink| {
                // Dropping the sender is what ends the worker's loop.
                drop(sink.queue.take());
                let (sent, error) = sink.worker.join().unwrap_or_else(|_| {
                    (0, Some("the thread feeding the sink panicked".to_owned()))
                });
                Delivery {
                    name: sink.spec.name,
                    required: sink.spec.required,
                    sent,
                    dropped: sink.dropped,
                    error,
                }
            })
            .collect()
    }
}

/// Writes queued events to a sink's program until the queue closes, then
/// waits for the program. Returns how many events it was given and why it
/// failed, if it did.
fn feed(mut child: Child, events: &Receiver<String>, grace: Duration) -> (u64, Option<String>) {
    let mut input = child.stdin.take();
    // The program's own complaints are read as they come, so a chatty sink
    // cannot fill its pipe and stall. The last line is what a failure shows.
    let complaints = child.stderr.take().map(|stderr| {
        thread::spawn(move || {
            BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
                .filter(|line| !line.trim().is_empty())
                .last()
        })
    });
    let mut sent = 0u64;
    let mut broken: Option<String> = None;
    // Keep receiving after a failure, so the run's side never finds the
    // queue full because of a sink that has already gone.
    for line in events {
        if broken.is_some() {
            continue;
        }
        let wrote = match input.as_mut() {
            Some(input) => input
                .write_all(line.as_bytes())
                .and_then(|()| input.write_all(b"\n"))
                .and_then(|()| input.flush()),
            None => Ok(()),
        };
        match wrote {
            Ok(()) => sent += 1,
            Err(e) => broken = Some(format!("it stopped reading events: {e}")),
        }
    }
    drop(input.take());
    let deadline = Instant::now() + grace;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!(
                    "it was still running {} s after the run's last event, and was stopped",
                    grace.as_secs()
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(e) => break Err(format!("waiting for it: {e}")),
        }
    };
    let said = complaints
        .and_then(|reader| reader.join().ok())
        .flatten()
        .map(|line| {
            let line: String = line.chars().take(200).collect();
            format!(": {line}")
        })
        .unwrap_or_default();
    let error = match status {
        Err(why) => Some(why),
        Ok(status) if !status.success() => Some(match status.code() {
            Some(code) => format!("it exited with status {code}{said}"),
            None => format!("it was stopped by a signal{said}"),
        }),
        Ok(_) => broken,
    };
    (sent, error)
}

/// Splits `name.key=value` settings by sink name. A setting for a sink that
/// was not asked for is an error: it is nearly always a misspelling.
pub fn settings_by_sink(
    names: &[String],
    settings: &[String],
) -> Result<BTreeMap<String, BTreeMap<String, String>>, String> {
    let mut by_sink: BTreeMap<String, BTreeMap<String, String>> = names
        .iter()
        .map(|name| (name.clone(), BTreeMap::new()))
        .collect();
    for setting in settings {
        let parsed = setting.split_once('=').and_then(|(key, value)| {
            let (sink, key) = key.split_once('.')?;
            (!sink.is_empty() && !key.is_empty()).then_some((sink, key, value))
        });
        let Some((sink, key, value)) = parsed else {
            // The value may be a credential, so only what precedes it is shown.
            let shown = setting.split('=').next().unwrap_or_default();
            return Err(format!(
                "--sink-config {shown:?} is not SINK.KEY=VALUE, as in webhook.url=https://example.com/hook"
            ));
        };
        match by_sink.get_mut(sink) {
            Some(settings) => {
                settings.insert(key.to_owned(), value.to_owned());
            }
            None => {
                return Err(format!(
                    "--sink-config {sink}.{key} is for a sink that was not asked for; add --sink {sink}"
                ))
            }
        }
    }
    Ok(by_sink)
}

/// Works out every sink asked for by name, with its settings.
pub fn specs(
    names: &[String],
    settings: &[String],
    dirs: &[PathBuf],
) -> Result<Vec<SinkSpec>, String> {
    let mut seen: Vec<&String> = Vec::new();
    for name in names {
        if seen.contains(&name) {
            return Err(format!("the sink {name:?} was asked for twice"));
        }
        seen.push(name);
    }
    let mut by_sink = settings_by_sink(names, settings)?;
    names
        .iter()
        .map(|name| SinkSpec::find(name, dirs, by_sink.remove(name).unwrap_or_default()))
        .collect()
}
