# Plugin settings

Everything an operator can configure about how each target's runner drives
its tool, as each plugin itself declares it. This page is generated:
`crates/meridian-cli/tests/plugin_settings.rs` writes it from the plugins and
fails when it is out of date, so it cannot drift from the code.
`meridian plugins --settings` prints the same list for the plugins it finds.

A setting is read from the first of these that has it:

1. **The environment variable**, when it is set and not blank. It always wins,
   so a shell or a CI job can override what the project says.
2. **The project's `meridian.yaml`**, under `spec.settings.<target>.<name>`,
   using the short name in the first column. The settings travel with the
   project, and a name the plugin does not declare refuses the run before the
   plugin starts. See [the manifest](modules.md#plugin-settings).
3. **The default** in the third column; *none* means the setting is off or
   empty until set.

An argument list (`args`, `ssh-args`, `kubectl-args` and the like) is split
the way a POSIX shell splits a command line: words are separated by blanks,
`"..."` and `'...'` hold a word together, and a backslash outside single
quotes takes the next character as it is. So `--extra-vars "a=b c"` is two
arguments, the second holding a space, and a path with a space in it can be
quoted. A quote that is never closed takes the rest of the text.

A value that is a secret, such as a token, does not belong in a setting or in
`meridian.yaml`: declare it under `spec.secrets` in the document, and the
target hands it to its tool by a private channel. See
[Secrets](dsl/secrets.md#handing-a-secret-to-a-tool).

A setting that two targets share, such as `MERIDIAN_AZ_ARGS` for `arm` and
`bicep`, is one variable read by both plugins; under `spec.settings` it is set
for each target by name.

## [ansible](../targets/ansible.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `program` | `MERIDIAN_ANSIBLE_PLAYBOOK` | `ansible-playbook` | The `ansible-playbook` program to run, when it is not on `PATH`. |
| `args` | `MERIDIAN_ANSIBLE_ARGS` | none | Extra arguments for every run, split like a shell command line: how to connect, which user, which key. For example `--user deploy --private-key ~/.ssh/deploy`. |
| `doc` | `MERIDIAN_ANSIBLE_DOC` | `ansible-doc` | The `ansible-doc` program `meridian schema sync` runs to describe the modules the documents use, when it is not on `PATH`. |
| `galaxy` | `MERIDIAN_ANSIBLE_GALAXY` | `ansible-galaxy` | The `ansible-galaxy` program `meridian schema sync` runs to read the installed collections' versions, when it is not on `PATH`. |

## [arm](../targets/arm.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `program` | `MERIDIAN_AZ` | `az` | The Azure CLI program to run, when it is not `az` on `PATH`. Shared with the bicep target. |
| `args` | `MERIDIAN_AZ_ARGS` | none | Extra arguments for every `az` call, split like a shell command line: `--only-show-errors`. Shared with the bicep target. |
| `parameters` | `MERIDIAN_AZ_PARAMETERS` | none | Values for the template's parameters, split like a shell command line. Each is given to `--parameters`: `@prod.parameters.json adminUser=ops`. |

## [bicep](../targets/bicep.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `program` | `MERIDIAN_AZ` | `az` | The Azure CLI program to run, when it is not `az` on `PATH`. Shared with the arm target. |
| `args` | `MERIDIAN_AZ_ARGS` | none | Extra arguments for every `az` call, split like a shell command line: `--only-show-errors`. Shared with the arm target. |

## [chef](../targets/chef.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `client` | `MERIDIAN_CHEF_CLIENT` | `chef-client` | The `chef-client` program, when it is not on `PATH`. Cinc's `cinc-client` takes the same arguments. |
| `run` | `MERIDIAN_CHEF_RUN` | `chef-run` | The `chef-run` program, when it is not on `PATH`. |
| `transport` | `MERIDIAN_CHEF_TRANSPORT` | none | `local` or `remote`, to override the choice made from the host: `local` converges this machine with chef-client, `remote` reaches the host with chef-run. |
| `args` | `MERIDIAN_CHEF_ARGS` | none | Extra arguments for every run, split like a shell command line. |
| `inspector` | `MERIDIAN_CHEF_INSPECTOR` | `chef-resource-inspector` | The resource inspector `meridian schema sync` runs to read what each resource takes, when it is not `chef-resource-inspector` on `PATH`; `cinc-resource-inspector`, then the copies the Cinc and Chef packages keep under `/opt/cinc` and `/opt/chef`, are tried when that does not run. |
| `ssh-agent` | `MERIDIAN_SSH_AGENT` | `ssh-agent` | The `ssh-agent` program, when it is not on `PATH`. It is started for the length of a stage when the document reaches its host as a credential with a private key, which the tool is then given through the agent and nothing else. |
| `ssh-add` | `MERIDIAN_SSH_ADD` | `ssh-add` | The `ssh-add` program, when it is not on `PATH`. It loads the credential's key into the agent from its standard input. |

## [cloud-init](../targets/cloud-init.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `program` | `MERIDIAN_CLOUD_INIT` | `cloud-init` | The `cloud-init` program to run, when it is not on `PATH`. |
| `validate` | `MERIDIAN_CLOUD_INIT_VALIDATE` | `on` | `on` or `off`: whether a plan and an apply check the user-data against cloud-init's schema. Set it to `off` on a machine that has no cloud-init to run the check with. |
| `wait` | `MERIDIAN_CLOUD_INIT_WAIT` | `off` | `ssh` or `off`: whether an apply waits, over SSH, for the host's first boot to finish. |
| `ssh` | `MERIDIAN_CLOUD_INIT_SSH` | `ssh` | The `ssh` program to wait with, when it is not on `PATH`. |
| `ssh-args` | `MERIDIAN_CLOUD_INIT_SSH_ARGS` | none | Extra arguments for `ssh`, split like a shell command line: which user, which key. For example `-l ubuntu -i ~/.ssh/deploy`. |

## [cloudformation](../targets/cloudformation.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `program` | `MERIDIAN_AWS` | `aws` | The AWS CLI program to run, when it is not `aws` on `PATH`. |
| `args` | `MERIDIAN_AWS_ARGS` | none | Extra arguments for every `aws` call, split like a shell command line: `--profile prod --region eu-west-1`. |

## [docker](../targets/docker.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `program` | `MERIDIAN_DOCKER` | `docker` | The `docker` program to run, when it is not on `PATH`. |
| `args` | `MERIDIAN_DOCKER_ARGS` | none | Extra arguments for `docker compose up`, split like a shell command line: `--pull always`, `--force-recreate`. |

## [dsc](../targets/dsc.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `program` | `MERIDIAN_PWSH` | `pwsh` | The PowerShell program, when it is not `pwsh` on `PATH`. |
| `module` | `MERIDIAN_DSC_MODULE` | `Dsc.PipelineRunner` | The DscPipelineRunner module to import: a name PowerShell can find, or the path of its manifest. |

## [helm](../targets/helm.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `helm` | `MERIDIAN_HELM` | `helm` | The `helm` program to run, when it is not on `PATH`. |
| `helm-args` | `MERIDIAN_HELM_ARGS` | none | Extra arguments for every `helm` call, split like a shell command line: `--kube-context prod`. |
| `kustomize` | `MERIDIAN_KUSTOMIZE` | `kustomize build` | The command that builds an overlay, when it is not `kustomize build`: `kubectl kustomize` uses the copy built into kubectl. |
| `kubectl` | `MERIDIAN_KUBECTL` | `kubectl` | The `kubectl` program that reads outputs, when it is not on `PATH`. Shared with the kubernetes target. |
| `kubectl-args` | `MERIDIAN_KUBECTL_ARGS` | none | Extra arguments for every `kubectl` call, split like a shell command line: `--context prod`. Shared with the kubernetes target. |

## [kubernetes](../targets/kubernetes.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `program` | `MERIDIAN_KUBECTL` | `kubectl` | The `kubectl` program to run, when it is not on `PATH`. Shared with the helm target. |
| `args` | `MERIDIAN_KUBECTL_ARGS` | none | Extra arguments for every `kubectl` call, split like a shell command line: `--context prod`. Shared with the helm target. |
| `output-wait` | `MERIDIAN_KUBECTL_OUTPUT_WAIT` | `120` | How many seconds an apply waits for an output the cluster has not filled in yet, such as a load balancer's address. |
| `wait-timeout` | `MERIDIAN_KUBECTL_WAIT_TIMEOUT` | `300` | How many seconds an apply waits for each object of a `dependsOn` wave to be ready (a rollout, a Job, a CustomResourceDefinition) before it fails with `timed-out`. |

## [opentofu](../targets/opentofu.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `program` | `MERIDIAN_TOFU` | `tofu` | The `tofu` program to run, when it is not on `PATH`. |
| `args` | `MERIDIAN_TOFU_ARGS` | none | Extra arguments for `tofu init`, `tofu plan` and `tofu apply`, split like a shell command line: `-parallelism=4`, `-lock-timeout=30s`. The calls that read a saved plan or the outputs get none. |

## [openvox](../targets/openvox.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `openvox` | `MERIDIAN_OPENVOX` | none | OpenVox's `puppet` program, when it is not on `PATH` or in the places the OpenVox packages install it. |
| `openbolt` | `MERIDIAN_OPENBOLT` | none | OpenBolt's `bolt` program, when it is not on `PATH` or in the places the OpenBolt packages install it. |
| `transport` | `MERIDIAN_OPENVOX_TRANSPORT` | none | `local` or `bolt`, to override the choice made from the host: `local` applies on this machine, `bolt` reaches the host with OpenBolt. |
| `args` | `MERIDIAN_OPENVOX_ARGS` | none | Extra arguments for every run, split like a shell command line. |
| `verify` | `MERIDIAN_OPENVOX_VERIFY` | `warn` | `warn`, `strict` or `off`: what to do when the tool turns out not to be OpenVox. `warn` says so and runs it, `strict` refuses, `off` does not ask. |
| `ssh-agent` | `MERIDIAN_SSH_AGENT` | `ssh-agent` | The `ssh-agent` program, when it is not on `PATH`. It is started for the length of a stage when the document reaches its host as a credential with a private key, which the tool is then given through the agent and nothing else. |
| `ssh-add` | `MERIDIAN_SSH_ADD` | `ssh-add` | The `ssh-add` program, when it is not on `PATH`. It loads the credential's key into the agent from its standard input. |

## [packer](../targets/packer.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `program` | `MERIDIAN_PACKER` | `packer` | The `packer` program to run, when it is not on `PATH`. |
| `args` | `MERIDIAN_PACKER_ARGS` | none | Extra arguments for `packer validate` and `packer build`, split like a shell command line: `-only=amazon-ebs.web`, `-parallel-builds=1`. |

## [podman](../targets/podman.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `podman` | `MERIDIAN_PODMAN` | `podman` | The `podman` program on the host, when it is not on its `PATH`. |
| `systemctl` | `MERIDIAN_PODMAN_SYSTEMCTL` | `systemctl` | The `systemctl` program on the host. |
| `scope` | `MERIDIAN_PODMAN_SCOPE` | `user` | `user` runs the units as the connecting user's own, without root. `system` installs them for the whole machine, using `sudo` when the connecting user is not root. |
| `unit-dir` | `MERIDIAN_PODMAN_UNIT_DIR` | none | The directory unit files are installed in, when it is not the standard one for the scope. |
| `quadlet` | `MERIDIAN_PODMAN_QUADLET` | none | Where Quadlet's generator is on the host, when it is not in one of the places distributions put it. |
| `ssh` | `MERIDIAN_PODMAN_SSH` | `ssh` | The `ssh` program used to reach another host. |
| `ssh-args` | `MERIDIAN_PODMAN_SSH_ARGS` | none | Extra arguments for every `ssh`, split like a shell command line: which user, which key, which port. For example `-l deploy -i ~/.ssh/deploy`. |
| `ssh-agent` | `MERIDIAN_SSH_AGENT` | `ssh-agent` | The `ssh-agent` program, when it is not on `PATH`. It is started for the length of a stage when the document reaches its host as a credential with a private key, which the tool is then given through the agent and nothing else. |
| `ssh-add` | `MERIDIAN_SSH_ADD` | `ssh-add` | The `ssh-add` program, when it is not on `PATH`. It loads the credential's key into the agent from its standard input. |

## [pulumi](../targets/pulumi.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `program` | `MERIDIAN_PULUMI` | `pulumi` | The `pulumi` program to run, when it is not on `PATH`. |
| `args` | `MERIDIAN_PULUMI_ARGS` | none | Extra arguments for `pulumi preview`, `pulumi up` and `pulumi destroy`, split like a shell command line: `--parallel 4`. The calls that select the stack or read its outputs get none. Pulumi's own `PULUMI_BACKEND_URL` and `PULUMI_CONFIG_PASSPHRASE` are read as Pulumi reads them. |
| `aws` | `MERIDIAN_PULUMI_AWS` | `aws` | The `aws` program a Meridian-run notify on an AWS resource calls, when it is not on `PATH`. |

## [puppet](../targets/puppet.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `puppet` | `MERIDIAN_PUPPET` | `puppet` | The `puppet` program, when it is not on `PATH`. |
| `bolt` | `MERIDIAN_BOLT` | `bolt` | The `bolt` program, when it is not on `PATH`. |
| `transport` | `MERIDIAN_PUPPET_TRANSPORT` | none | `local` or `bolt`, to override the choice made from the host: `local` applies on this machine with `puppet apply`, `bolt` reaches the host with `bolt apply`. |
| `args` | `MERIDIAN_PUPPET_ARGS` | none | Extra arguments for every run, split like a shell command line. |
| `ssh-agent` | `MERIDIAN_SSH_AGENT` | `ssh-agent` | The `ssh-agent` program, when it is not on `PATH`. It is started for the length of a stage when the document reaches its host as a credential with a private key, which the tool is then given through the agent and nothing else. |
| `ssh-add` | `MERIDIAN_SSH_ADD` | `ssh-add` | The `ssh-add` program, when it is not on `PATH`. It loads the credential's key into the agent from its standard input. |

## [salt](../targets/salt.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `salt-call` | `MERIDIAN_SALT_CALL` | `salt-call` | The `salt-call` program, when it is not on `PATH`. |
| `salt-ssh` | `MERIDIAN_SALT_SSH` | `salt-ssh` | The `salt-ssh` program, when it is not on `PATH`. |
| `transport` | `MERIDIAN_SALT_TRANSPORT` | none | `local` or `ssh`, to override the choice made from the host: `local` applies on this machine with `salt-call --local`, `ssh` reaches the host with `salt-ssh`. |
| `args` | `MERIDIAN_SALT_ARGS` | none | Extra arguments for every run, split like a shell command line. For example `--config-dir /srv/salt-config` locally, or `--user deploy --priv /home/me/.ssh/deploy -i` over SSH. |
| `ssh-agent` | `MERIDIAN_SSH_AGENT` | `ssh-agent` | The `ssh-agent` program, when it is not on `PATH`. It is started for the length of a stage when the document reaches its host as a credential with a private key, which the tool is then given through the agent and nothing else. |
| `ssh-add` | `MERIDIAN_SSH_ADD` | `ssh-add` | The `ssh-add` program, when it is not on `PATH`. It loads the credential's key into the agent from its standard input. |

## [terraform](../targets/terraform.md)

| Setting | Variable | Default | What it does |
|---|---|---|---|
| `program` | `MERIDIAN_TERRAFORM` | `terraform` | The `terraform` program to run, when it is not on `PATH`. OpenTofu's `tofu` takes the same commands. |
| `args` | `MERIDIAN_TERRAFORM_ARGS` | none | Extra arguments for `terraform init`, `terraform plan` and `terraform apply`, split like a shell command line: `-parallelism=4`, `-lock-timeout=30s`. The calls that read a saved plan or the outputs get none. |

