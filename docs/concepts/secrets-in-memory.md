# How a secret is held in memory

A password that Meridian fetches for a tool exists in Meridian's memory for a
while. This page says what is done to it during that while, what is not
promised, and where the code is. The design, with what is still to be built, is
in [the language plan](../plans/language-expansion.md#115-holding-a-secret-in-memory).

Everything here lives in one crate, `meridian-secret`. No other crate holds
the bytes of a secret.

<p align="center"><img src="../graphics/meridian-secrets-flow.svg" alt="How a secret moves: from a provider into locked memory, as a reference everywhere else, and to the tool by a private file at run time" width="100%"></p>

> **Where this stands.** A document can declare secrets and read them: see
> [the secrets reference](../reference/dsl/secrets.md). A secret can decide a
> `when` today, `meridian secret check` says whether each would be found, and
> a credential can be resolved outside a run, a secret can name several
> places and be kept in the operating system's own stores, and
> `meridian secret set` puts it there. A secret in a resource's params is
> handed to the tool by a private file on the targets that
> [take one](../reference/dsl/secrets.md#handing-a-secret-to-a-tool), and a
> [credential's](../reference/dsl/credentials.md) secret fields the same way.

## The promise, and its limit

Meridian keeps a secret out of everything it writes (logs, plan output, events,
the run record, panic messages, error messages), out of swap and core dumps,
out of the environment and arguments of the programs it starts, and out of its
own memory once the secret is no longer needed.

It does not protect a secret from `root`, from a debugger attached by `root`,
from a compromised kernel, or from the tool the secret is handed to. A tool
that is given a password has the password.

## One home for each secret

A secret is a `SecretValue`. Its bytes are in a block of memory mapped for that
secret alone, of its final size from the start, so the bytes are never moved or
left behind in freed memory.

| Property | Linux | macOS and other Unix | Windows |
|---|---|---|---|
| Wiped when dropped (`zeroize`, a write the compiler cannot remove) | yes | yes | yes |
| Locked out of swap (`mlock`) | yes | yes | not yet |
| Guard page on each side, so an overrun faults | yes | yes | not yet |
| Left out of core dumps (`MADV_DONTDUMP`) | yes | no | no |
| Zeroes in a forked child (`MADV_WIPEONFORK`) | yes | no | no |

`SecretValue::protection()` reports which of these were achieved for a value,
so a run can say so once instead of assuming. Locking can fail when the limit
on locked memory (`RLIMIT_MEMLOCK`) is small; the value is still created, and
`protection().locked` is false. A run that holds such a secret
[warns once](../reference/cli.md#secrets), naming it, and with
`--strict-secrets` stops the stage instead.

A `SecretValue` cannot be printed, copied or compared by accident:

- `Debug` and `Display` both give `(sensitive)`, so a `{:?}` on a structure
  that holds one, an `unwrap` on an error that holds one and a panic message
  all print that and nothing else.
- It does not implement `Clone`, `PartialEq`, `Hash`, `Serialize` or `Deref`.
  A copy is `duplicate()`, which makes a second locked block. A comparison is
  `same()`, which takes the same time whatever the bytes are.
- The bytes are reached only through `expose(|bytes| ...)`, which lends them to
  a closure. A new secret made from an old one (an encoded form, a secret with
  text around it) is `derive()`, which writes straight into a new locked block.

## Few places expose a secret

Every call to `expose` is a place a secret can leave its protection. A test,
`crates/meridian-secret/tests/expose_is_rare.rs`, reads the workspace's sources
and fails when a call appears in a file that is not on its list. Adding a call
site means adding the file to that list, with the reason, in review.

The same test keeps `unsafe` code in one module, `sys.rs`. The rest of the
workspace forbids it.

## The process

`harden_process()` is called before any secret is read. It sets the core dump
limit to zero (`setrlimit(RLIMIT_CORE, 0)`) and, on Linux, makes the process
undumpable (`prctl(PR_SET_DUMPABLE, 0)`), which also stops another process of
the same user from attaching a debugger or reading `/proc/<pid>/mem`. It
returns what it achieved, and `hardening()` reads the same facts back from the
system.

Being undumpable is a setting of the whole process, and it has a side effect
worth knowing: the kernel gives the files under `/proc/<pid>` to `root`, so
the same user can no longer read them. A tool that inspects a running
`meridian` (reading `/proc/<pid>/environ` or `/proc/<pid>/fd`, attaching
`strace` or `gdb`) has to be run as `root`. Programs Meridian starts are not
affected: the setting is reset when a program is executed.

## Everywhere else: a reference, not the value

The rest of Meridian never holds a secret's bytes. A value that is a secret
(`Value::Secret`) holds a number: which secret of the run is meant. The bytes
are in the run's `SecretTable`, in this crate.

- The reference prints, compares and encodes as `(sensitive)`. A plan, a log
  line, an error, a debug dump and the run's state are all made from values,
  so none of them can show a secret: the bytes were never in what they are
  made from. This is stronger than masking output afterwards, which fails the
  first time someone forgets.
- An expression that joins a secret to text or encodes it asks the table to
  do it. The work is done on protected memory and the answer is another
  reference.
- A secret that has not been fetched (`validate` and `compile` never fetch
  one) is in the table as its name. What is made from it is a description,
  such as `password=(secret db_password)`.
- The table for a stage is wiped when the stage ends. A reference that
  outlives it names nothing.

## Fetching

Seven providers are built. Each reads straight into locked memory, and none
puts a value into an error.

| Provider | What it does | What it refuses |
|---|---|---|
| `from_env(key)` | reads the variable, **removes it from the environment** so a program started later does not inherit it, wipes the copy it read, and keeps one copy in locked memory so a later read of the same variable in the same process still answers | an unset variable is `Missing`; an empty one is an empty secret, not a missing one |
| `from_file(path, options)` | reads the file, without its last line ending unless `keep_trailing_newline` | a file that anyone but its owner can read or write, unless `allow_wide_mode`; a file longer than 256 KiB; a directory |
| `from_exec(command)` | runs the command **without a shell**, reads what it prints, discards its standard error | a command that fails, is not found, is stopped by a signal, or prints nothing. The error names the program and never its arguments or output |
| `from_keyring(description)` | finds the key with `keyctl(KEYCTL_SEARCH)` in the session and then the user keyring, and reads it with `KEYCTL_READ` **straight into the locked region**, sized by the key's own length; no library buffer is between the kernel and the home | a key larger than 256 KiB; a description with a NUL. The keyring being shut off (a container's sandbox) is `Unavailable` |
| `from_systemd_credential(name, keep)` | opens `$CREDENTIALS_DIRECTORY/<name>` and reads it as a file | a name that is a path; no `CREDENTIALS_DIRECTORY` is `Unavailable` |
| `from_secret_service(attributes)` | runs `secret-tool lookup` and reads its standard output as `exec` does | no session bus, or no `secret-tool`, is `Unavailable`; the tool failing or printing nothing is `Missing` |
| `from_keychain(service, account)` | runs `/usr/bin/security find-generic-password -w` and reads its standard output | exit status 44 is `Missing`; any other failure is refused without what the tool printed; not macOS is `Unavailable` |

`Source::store(label, value)` writes the two stores Meridian writes: the
kernel keyring with `add_key(2)` from the locked region, and the Secret
Service by handing the value to `secret-tool store` on its standard input.
Nothing goes through an argument.

A third kind of answer, `FetchError::Unavailable`, is what lets a secret name
several places: `fetch_first` passes over a place that is `Missing` or
`Unavailable` for the next and stops at one that `Failed`, and `probe_first`
and `store_first` do the same without reading or with writing. The value a
person types for `secret set` is read by `read_from_stdin` with `read(2)` on
the terminal, a byte at a time with echo off, into the locked region, with no
`BufReader` keeping a copy.

Where a secret comes from is a `Source`: the provider and what it needs. A
document's declaration and a project's credential are both made of them, so
both are checked by the same rules (`Source::problems()`) and fetched the same
way (`Source::fetch()`). `Source::probe()` says whether the place would
answer without reading it, which is what `meridian secret check` prints.

## A credential outside a run

Some work needs an identity when no document is being run: fetching a module
from a private registry, for one. `resolve_credential(declared, name)` answers
it with the same model.

- A `CredentialDeclaration` is a name, a type and fields. A field that is not
  secret holds its text. A field that is secret holds a `Source`, never a
  value: a secret written out is refused, and the refusal does not repeat it.
- Three types can be resolved today: `registry` (`server`, `user`,
  `password`), `usernamePassword` (`user`, `domain`, `password`) and `token`
  (`token`, `scheme`). The rest of [the plan's list](../plans/language-expansion.md#12-credentials)
  arrive with credentials in documents.
- Resolving fetches the secret fields of that one credential and no other,
  into locked memory. The `Credential` that comes back prints as its name and
  type, lends a secret field as a `SecretValue`, and wipes them when dropped.
- An error names the field and the provider, and nothing a provider read.

The first user is the module system: a project declares credentials under
`spec.credentials` in `meridian.yaml` and names one for a registry. See
[registry logins](../reference/modules.md#registry-logins).

## A credential in a run

A document declares the same kind of thing under `spec.credentials`, with
every type the language has, and names one for its host with
`targetHost.credential` ([Credentials](../reference/dsl/credentials.md)).
In a run a credential's secret fields are secrets of the document, and
nothing more is added to the model:

- A secret field is declared as exactly `${{ secret.<name> }}`, so once the
  document is resolved the field *is* a `Value::Secret` slot in the run's
  table: one home, as for any secret. A plain field (a user, a region) is
  text, checked as shown text, so a secret cannot be placed in one.
- The credential handed to a plugin is a `HeldCredential`: the declared
  shape, and one `SecretValue` per secret field, duplicated from the table
  by name into a holder of its own, wiped with the stage. It prints as its
  name, its type and its field names.
- It crosses to the plugin as the run context does: the values at plan and
  apply only, over the private socket, taken straight into protected memory
  on the other side (`SecretValue::take_from`); `outputs` is sent none. At
  compile the plugin sees the shape and the names of the secret fields, and
  no value.
- What the host prints and records is the summary, `deploy (ssh, user
  deploy)`: the plan line, the run record and the events hold that and
  nothing of a value.
- A rollback fetches the credential again at the stage's turn, as it does the
  document's secrets.

A credential's secret fields are counted among the stage's secrets for
masking what the tool prints, since they are the same slots.

## Handing a secret to a tool

In order of preference: the tool's standard input or a file descriptor, then a
file only the user can read, then (only where a tool accepts nothing else) the
environment of that one tool. Never an argument: arguments are visible to every
user in the process list.

`SecretFile::create(name, parts)` is the file:

- It is made in a directory of its own, `meridian-secrets-<pid>-<random>`, of
  mode `0700`, and the file is mode `0600` and must not already exist.
- The directory is on a memory filesystem where there is one
  (`XDG_RUNTIME_DIR`, `/run/user/<uid>`, `/dev/shm`), so the secret does not
  reach a disk. `in_memory()` says whether it is.
- The file is built from parts, literal text and secrets in order, so a
  secret goes from locked memory to the file without becoming a `String`.
- `beside(name, text)` writes a second file of the same protection into the
  same directory, for a tool that reaches the secret file only through
  another: a `hiera.yaml` naming its data directory, a pillar top file, a
  `client.rb`. The text holds no secret; the file goes with the directory.
- Dropping it overwrites the file and removes it and its directory, including
  anything a tool left beside it.
- A run that was killed cannot clean up. `sweep_stale()` removes the
  directories of processes that no longer exist, and leaves those of live ones.

## Masking what a tool prints

A tool may print the secret it was given. `Masker` is given each secret of a
run and replaces it with `(sensitive)` in the tool's output before Meridian
shows or records a line. It finds:

- the secret itself;
- its base64 forms (standard and URL-safe, with and without padding), its
  hexadecimal form and its URL-encoded form;
- each line of a secret that has several, since a tool that echoes a key
  prints it a line at a time.

A secret or a line shorter than four bytes is not searched for: masking every
`a` in the output would hide the output and reveal the secret's length.
`add()` returns `false` for one, so a run can say that it is not masked.

Masking is a second line of defence. The first is not handing a tool's output
anything to print: the delivery channels above.

## Across the plugin boundary

A target plugin is a separate process, so a secret handed to its tool
crosses one boundary on the way. What crosses, and when:

- **At compile, a name.** A resource's params reach the plugin with
  `Value::Secret` where a secret stands, and the graph names each one
  (`ResourceGraph::secrets`). The plugin writes a reference by that name into
  its artifact (`secrets_written_as`) and nothing else: it has no value to
  write. The message carries `(sensitive)` where the secret is and, beside
  it, the path to that place and the secret's slot, so the mask is put back
  as a `Value::Secret` on the other side.
- **At plan and apply, the value.** The run context carries the value of
  each secret the params refer to, over the plugin's private socket and
  nowhere else, and only to a target that declared it takes secrets. On the
  plugin's side the SDK takes each value straight into protected memory and
  wipes the message's bytes (`SecretValue::take_from`); on the host's side
  the message is the one copy outside protected memory a secret passes
  through, dropped as soon as it is sent. `outputs` is sent none.
- **For the length of the call.** The plugin's `RunContext::secrets` holds
  the values, each a `SecretValue` with the same missing `Debug` and
  `Display`, and they are wiped when the call's context is dropped.
- **To the tool by a private channel.** `Secrets::json_file` writes every
  value into one `SecretFile` under the names the artifact uses, for a tool
  that reads variables from a file; a plugin whose tool reads another format
  (Hiera's YAML, a Salt pillar) builds the `SecretFile` from parts the same
  way; `Tool::stdin_secret` pipes one into a tool's standard input. Neither
  the command line nor the environment is a channel the SDK offers, and a
  tool that has only the command line (`chef-run`) is refused the secret.
- **Rollback fetches again.** The run keeps nothing of a stage's secrets once
  the stage ends; a rollback at the end of a failed run, or `meridian
  rollback` later, fetches the document's secrets at the stage's turn.

What is not claimed: a plugin given a secret can do anything with it. The
defences are the ones for any code you run, a signed plugin from a named
publisher, and the plugin is given only the secrets of the resources it
compiles.

## What is tested

In `crates/meridian-secret/tests`:

| File | What it proves |
|---|---|
| `value.rs` | a value prints as `(sensitive)` everywhere, is locked and guarded, is compared and copied only by name |
| `wiped.rs` | after a secret is dropped its bytes are nowhere in the process: the test searches every writable mapping of its own memory |
| `process.rs` | hardening is what the system reports; a secret read from the environment is not inherited by the next program; five kinds of panic do not print a held secret |
| `provider.rs` | each provider's refusals, and that no error holds a value or an argument; a probe reads nothing and runs nothing |
| `deliver.rs` | the file's modes, content, removal, and the sweep after a killed run |
| `mask.rs` | every form is masked, in text and in bytes |
| `credential.rs` | a credential resolves with its secret fields in protected memory; only the one asked for is fetched; a secret written out is refused; no error or debug form holds a value |
| `stores.rs` | the kernel keyring gives back every byte stored in it, or says it is shut off; a unit credential is one name and never a path; the Secret Service is reached through its tool with the value on standard input and never in an argument, and is unavailable without a bus or the tool; the keychain is unavailable off macOS and read from a keychain made for the test on it; with several places the first that has the secret answers, an unavailable store is passed over, a refusal stops the search, and every reason is given when none answers |
| `expose_is_rare.rs` | `expose` and `unsafe` appear only where expected; the SDK's wire conversion and standard-input channel are the two places added for handing a secret to a plugin and its tool |

Across the other crates:

| File | What it proves |
|---|---|
| `meridian-secret/tests/table.rs` | secrets are joined, encoded and compared without leaving protected memory; one that is not fetched is described and never guessed; a wiped one is gone |
| `meridian-ir/tests/credentials.rs` | a credential's secret fields are one declared secret each, a literal is refused without being repeated, a plain field takes no secret, and a resolved credential holds slots and never values |
| `meridian-cli/tests/credentials.rs` | a credential's key reaches the test target in a private file of mode `0600`, and is in nothing `plan` and `apply` print or write: not the plan, not the state directory, not the run record; a target that takes none is refused before anything runs |
| `plugins/ansible/tests/run.rs` | a key reaches a stand-in `ansible-playbook` as `--private-key` naming a `0600` file, a password in the extra-vars file, and neither is in an argument, the environment or the log; the files are gone after the run |
| `meridian-expr/tests/secrets.rs` | an expression holds a reference; joins and placements stay secret; a comparison is a plain answer; every other function and operator refuses; no error holds a value |
| `meridian-ir/tests/secrets.rs` | every rule of a declaration; every place a secret is refused; a resolved document, its debug form and its encoding hold only `(sensitive)`; the secrets are wiped when their holder is dropped |
| `meridian-cli/tests/secret_stores.rs` | through the real command: `secret set` stores a value from standard input and a run reads it back from the keyring and the Secret Service; the value reaches the store's tool on its standard input and never as an argument; a store this machine does not have is passed over for the next place and every reason is given when none has it; the value is in nothing printed or written |
| `meridian-cli/tests/secrets.rs` | through the real command: the value is in nothing `plan` and `apply` print and in no file of the state directory or of `compile --out`; a failed fetch repeats neither the value, a command's output nor its arguments; a secret read from the environment is not inherited by a program started afterwards; a secret that cannot be locked is warned about once, or refused with `--strict-secrets`; `secret check` reads no value and runs no command; a secret in a param is compiled as a reference and handed to the tool in a file only the user can read, which the tool reads back whole, and is in nothing else the run prints, applies or keeps; a target that takes none stops the stage and names the ones that do |
| `meridian-sdk/src/wire.rs`, `src/secrets.rs`, `src/tool.rs` | a secret in a param crosses the wire as a place and a slot and never a value, and a place that does not hold the mask is refused; a run context carries its values into protected memory and `outputs` carries none; `Secrets` prints only names; a JSON variables file holds every value escaped under its key and refuses a secret that is not text; a secret on standard input reaches the tool and is not on its command line |
| `meridian-plugin-host/tests/runner.rs` | through the real plugin process: a secret in a param reaches the plugin by name at compile and by value when it runs, and the tool is handed it in a private file; a plugin that takes no secrets says so |
| `plugins/ansible/tests/run.rs`, `plugins/terraform/tests/run.rs`, `plugins/opentofu/tests/run.rs` | against a stand-in for the tool: the value reaches it in a file of mode `0600`, under the variable's name; the command line names the file and nothing more; the environment holds nothing; the file is gone when the run is over; Terraform's saved plan, which holds every variable, is scrubbed |
| `plugins/ansible/tests/real.rs`, `plugins/terraform/tests/real.rs`, `plugins/opentofu/tests/real.rs` | against the real tools in CI: the value reaches the play and the configuration and is in nothing the tool prints, in check mode with diff included, and in no file the plugin wrote |
| `plugins/packer/tests/run.rs`, `plugins/cloudformation/tests/run.rs`, `plugins/bicep/tests/run.rs`, `plugins/arm/tests/run.rs`, `plugins/pulumi/tests/run.rs` | against a stand-in for the tool: the value reaches it in a variables or parameters file of mode `0600`, or on standard input for Pulumi; the command line names the file or the key and nothing more; the environment holds nothing; the file is gone when the stage is over; Pulumi is told to forget the configuration value even when the preview fails |
| `plugins/packer/tests/packer.rs`, `plugins/cloudformation/tests/real.rs`, `plugins/bicep/tests/real.rs`, `plugins/arm/tests/real.rs`, `plugins/pulumi/tests/real.rs` | against the real tools in CI: a Packer step and a CloudFormation stack in LocalStack receive the value and show it nowhere; the Bicep compiler makes a `securestring` of the parameter and the ARM reader reads it back as `@secure()`; Pulumi keeps the value as a secret input and output and leaves nothing in the open |
| `plugins/kubernetes/tests/run.rs`, `plugins/helm/tests/run.rs`, `plugins/docker/tests/run.rs`, `plugins/podman/tests/run.rs` | against a stand-in for the tool: the manifests or values reach `kubectl` and `helm` on standard input with the value filled in, and no file holds it; Compose is given a `0600` environment file that quotes the value for its reader and is gone after the stage; Podman is given the value on the apply script's standard input and asked to create a secret from it, and a plan creates none; in every case the command line names no value and the environment holds none |
| `plugins/kubernetes/tests/real.rs`, `plugins/helm/tests/real.rs`, `plugins/docker/tests/real.rs`, `plugins/podman/tests/real.rs` | against the real tools in CI: the value is in the Secret a kind cluster holds and in the Secret Helm renders, reaches a Compose service's environment and its `/run/secrets` file, and reaches a container Podman started from a unit that names the secret; it is in no file the plugin wrote and in nothing printed |
| `crates/meridian-cli/tests/secret_conformance.rs` | every target, on its own example: a target that takes a secret compiles the reference it says it does, and one that takes none stops `plan` naming the ones that do; the value reaches no file either way |
