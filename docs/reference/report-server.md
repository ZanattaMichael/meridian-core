# The report server

`meridian-report-server` is an optional place to send your runs' events and
view them later: every project's runs in one list, each run's report as a page,
and every [report format](reporting.md#the-formats) over HTTP. Meridian runs
fine without it.

**Does:** receive events from the `server` [sink](reporting.md#sinks); keep
them; show runs per project, a run's report, a run in progress; serve a
run's JSON, JUnit, Markdown and HTML reports and its events.
**Does not:** start, schedule, approve or cancel runs; hold state about
your infrastructure; hold a value or a secret, because an event has none;
speak TLS (put it behind what already does); manage users (there are two
tokens, one to write and one to read).

## How it fits

```mermaid
flowchart LR
    subgraph ci["wherever runs happen"]
        RUN["meridian apply<br/>--sink server"] -->|"events, as they happen"| SINK["meridian-notify-server"]
    end
    SINK -->|"POST, with the write token"| SRV["meridian-report-server"]
    SRV --> DISK[("events.jsonl<br/>one file per run")]
    DISK -->|"folded when asked for"| PAGES["pages: projects, runs, a run"]
    DISK --> API["HTTP: a run's report in any format,<br/>its events, the list of runs"]
    PAGES --> YOU["a person"]
    API --> TOOLS["a dashboard, a script"]
```

The server stores only events, in the same layout as a run's
[record](state.md). Everything it shows is built from them on request by the
same code as `meridian report`, so the server, the command and the run's own
report always agree.

## Running it

```
MERIDIAN_SERVER_TOKEN="$(openssl rand -hex 32)" \
    meridian-report-server --listen 127.0.0.1:8717 --data /var/lib/meridian-reports
```

| Option | Environment | Default | Meaning |
|---|---|---|---|
| `--listen ADDR` | `MERIDIAN_SERVER_LISTEN` | `127.0.0.1:8717` | the address to listen on. Port `0` picks a free one; the first line printed says which |
| `--data DIR` | `MERIDIAN_SERVER_DATA` | `meridian-reports` | where the events are kept |
| `--token-file FILE` | `MERIDIAN_SERVER_TOKEN` holds the token itself | | the write token: what a sender must present to add events. **Required**, and at least 16 characters |
| `--read-token-file FILE` | `MERIDIAN_SERVER_READ_TOKEN` holds the token itself | reading is open | the read token: what a reader must present |
| `--keep-runs N` | | `1000` | how many runs each project keeps; the oldest are removed |

A token is read from a file or the environment and never from an argument,
which every user of the machine could see.

### Before you expose it

| | |
|---|---|
| TLS | the server speaks plain HTTP. Keep it on the loopback address behind a reverse proxy that terminates TLS, or on a private network. A token sent over plain HTTP across a network is a token given away |
| Reading | with no read token, anyone who can reach the server can read every run. Reports hold names, never values, but names of hosts, steps and resources are still worth protecting: set a read token unless the network already limits who can reach it |
| Writing | the write token lets its holder add events to any project. It cannot read a secret, change a run that is stored, or start anything. Give CI the write token only |
| Disk | a request may carry 4 MiB and a run 200,000 events; `--keep-runs` bounds each project. Projects are created by whoever holds the write token |

A browser is asked for the read token as a password; the user name is
ignored. A program sends `Authorization: Bearer <token>`. The write token
also reads.

## Sending runs to it

The `server` sink is [`examples/sinks/meridian-notify-server`](../../examples/sinks/meridian-notify-server),
a short script that needs `curl`. Put it in a plugin directory.

```
export MERIDIAN_NOTIFY_TOKEN=...            # the server's write token
meridian apply site.yaml --sink server \
    --sink-config server.url=https://reports.example.com \
    --sink-config server.project=shop
```

| Setting | Meaning |
|---|---|
| `url` | the server's address. Required |
| `project` | the project the run is filed under: letters, digits, `-` and `_`. `default` when not set |
| `token` | the write token. Required. Set it as `MERIDIAN_NOTIFY_TOKEN` in the environment, not with `--sink-config`, which would leave it in your shell history |
| `timeout` | seconds to allow each request; `10` by default |

Like every sink, it gets every event unless `server.on` narrows it, and never
delays a step. A server that is down is a warning, not a failed run, unless you
set `server.required=true`.

A run that was never sent can be sent later from its record, with nothing
but `curl`:

```
meridian events RUN | curl --fail --data-binary @- \
    -H "Authorization: Bearer $MERIDIAN_NOTIFY_TOKEN" \
    https://reports.example.com/v1/projects/shop/runs/RUN/events
```

## What it shows

| Page | Shows |
|---|---|
| `/` | every project, how many runs it has, and how its latest run went |
| `/projects/{project}` | the project's newest 200 runs, newest first, with counts by kind of run |
| `/projects/{project}/runs/{run}` | the run's [HTML report](reporting.md#html), with links to the other formats. While the run is going the page reloads itself every 3 seconds; it uses no script |

## HTTP interface

| Request | Answers |
|---|---|
| `POST /v1/projects/{project}/runs/{run}/events` | adds events, one JSON object per line. `200` with `{"accepted":N,"ignored":M}`. Needs the write token |
| `GET /v1/projects/{project}/runs` | the newest 200 runs as JSON: `run`, `mode`, `status`, `started`, `finished`, `steps`, `failed`, `changed`, `drifted` |
| `GET /v1/projects/{project}/runs/{run}/events` | the run's events, one per line, in order |
| `GET /v1/projects/{project}/runs/{run}/report/{format}` | the run's report: `json`, `junit`, `markdown` or `html` |
| `GET /healthz` | `ok`. Needs no token |

Sending events is idempotent: an event the run already has (by sequence number)
is ignored, so an unsure sender can resend. Events may arrive in any order and
are served in the order they happened.

| Status | Means |
|---|---|
| `400` | a line is not an event, an event belongs to a different run, or the project or run is not a plain name. Nothing from the request was stored |
| `401` | the token is missing or wrong |
| `404` | no such project, run or format |
| `413` | the request is over 4 MiB, or the run is full |

What is stored is each event written out again, not the bytes that arrived,
so a field an event does not have is never kept.

## What is on disk

```
<data>/projects/<project>/runs/<run>/events.jsonl
```

One file per run, one event per line. There is no database and no index:
back the directory up, move it, or read a run with
`meridian report --from <data>/projects/shop/runs/<run>/events.jsonl`.
Removing a run's directory removes the run.

The server reads nothing outside the data directory. Requested projects and
runs are matched only against the directory's listing, and a directory is read
only if it resolves inside the data directory, so planted links pointing
elsewhere are not followed.
