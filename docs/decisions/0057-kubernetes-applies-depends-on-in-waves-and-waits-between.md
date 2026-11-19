# Kubernetes applies `dependsOn` in waves and waits between them

| | |
|---|---|
| Area | Runs |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | the pull requests for issue 263: part 1 (Kubernetes), part 2 (Helm) |
| Where it shows | [kubernetes: Ordering](../targets/kubernetes.md#ordering), [helm: Ordered releases](../targets/helm.md#ordered-releases), [Dependencies and notifications](../concepts/dependencies.md), [plugin settings](../reference/plugin-settings.md#kubernetes) |

## Kubernetes (part 1)

- **Waves by depth**: an object goes one wave after the latest object it depends on. Only
  with a `dependsOn`; otherwise one `kubectl apply` as before.
- **Compiled, not computed at run time**: the compile writes `wave-<n>.yaml` and lists the
  waves, with what to wait for, in `cluster.json`. `manifests.yaml` stays whole for plan,
  inspect and destroy.
- **Waits** after every wave but the last: `kubectl rollout status` for a Deployment,
  StatefulSet or DaemonSet; a Job's conditions read until `Complete` or `Failed`
  (`kubectl wait` cannot stop on either); `kubectl wait --for=condition=Established` for a
  CustomResourceDefinition. Nothing for other kinds.
- **Timeout**: the setting `wait-timeout` (300 s) per object; `timed-out` on expiry.
- **A failure stops the apply**: later waves are not applied; the failure names the
  resource (`resource-failed` for a failed Job), and the changes already made are reported.
- **Plan and destroy are unchanged**: one server-side dry run, one `kubectl delete`.
- The compile warning that `kubectl` does not wait is gone.

## Helm (part 2)

- **Opt in on the release**: `ordering: releases` (default `kind`, Helm's own order). A
  release param, not a plugin setting, as it changes what the compile writes.
- **Split only when needed**: a `dependsOn` Helm's order does not honour makes one release
  per wave, `<release>-<n>`, by the same depth as Kubernetes. Otherwise one release.
- **Compiled**: `chart-<n>/` per release beside the whole `chart/` (which a render reads);
  `release.json` lists `parts` in install order.
- **Apply** installs each with `--wait`, in order; a failure stops it with the changes made
  so far. **Destroy** uninstalls last first. **Plan** sums each release's changes.
- **Refused**: a document still installed as one release (moving objects needs adopt,
  #269); a notify between two releases (the checksum reads a template of the same chart);
  a Kustomize overlay.
