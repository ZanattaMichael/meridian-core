# Sinks

A sink is a program that is sent a run's events as they happen, and forwards
them somewhere: a webhook, a chat channel, a collector. `command` is built
in and runs any program you name. Any other is a program named
`meridian-notify-<name>` in a plugin directory.

| File | What it is |
|---|---|
| [`meridian-notify-file`](meridian-notify-file) | appends every event to a file. It is the reference implementation, and the one the tests prove the contract against |
| [`meridian-notify-server`](meridian-notify-server) | sends every event to a [Meridian report server](../../docs/reference/report-server.md) with `curl` |
| [`meridian-notify-webhook`](meridian-notify-webhook) | posts every event to an HTTP endpoint with `curl`, signed with a shared secret when one is set |

```
meridian apply site.yaml \
    --plugin-dir /usr/local/lib/meridian --plugin-dir examples/sinks \
    --sink webhook \
    --sink-config webhook.url=https://example.com/meridian \
    --sink-config webhook.on='run.*,step.failed'
```

The contract a sink keeps is in
[the reporting reference](../../docs/reference/reporting.md#writing-a-sink).
An event holds no value a document or a tool produced, so a sink cannot leak
a secret it was never sent.
