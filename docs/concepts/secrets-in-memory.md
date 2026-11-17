# How a secret is held in memory

A password that Meridian fetches for a tool exists in Meridian's memory for a
while. This page says what is done to it during that while, what is not
promised, and where the code is. The design, with what is still to be built, is
in [the language plan](../plans/language-expansion.md#115-holding-a-secret-in-memory).

Everything here lives in one crate, `meridian-secret`. No other crate holds
the bytes of a secret.

> **Where this stands.** A document can declare secrets and read them: see
> [the secrets reference](../reference/dsl/secrets.md). A secret can decide a
> `when` today, `meridian secret check` says whether each would be found, and
> a credential can be resolved outside a run. Handing a secret to a tool, the
> operating system's stores as providers and `meridian secret set` come next.

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

Three providers are built. Each reads straight into locked memory, and none
puts a value into an error.

| Provider | What it does | What it refuses |
|---|---|---|
| `from_env(key)` | reads the variable, **removes it from the environment** so a program started later does not inherit it, wipes the copy it read, and keeps one copy in locked memory so a later read of the same variable in the same process still answers | an unset variable is `Missing`; an empty one is an empty secret, not a missing one |
| `from_file(path, options)` | reads the file, without its last line ending unless `keep_trailing_newline` | a file that anyone but its owner can read or write, unless `allow_wide_mode`; a file longer than 256 KiB; a directory |
| `from_exec(command)` | runs the command **without a shell**, reads what it prints, discards its standard error | a command that fails, is not found, is stopped by a signal, or prints nothing. The error names the program and never its arguments or output |

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
| `expose_is_rare.rs` | `expose` and `unsafe` appear only where expected |

Across the other crates:

| File | What it proves |
|---|---|
| `meridian-secret/tests/table.rs` | secrets are joined, encoded and compared without leaving protected memory; one that is not fetched is described and never guessed; a wiped one is gone |
| `meridian-expr/tests/secrets.rs` | an expression holds a reference; joins and placements stay secret; a comparison is a plain answer; every other function and operator refuses; no error holds a value |
| `meridian-ir/tests/secrets.rs` | every rule of a declaration; every place a secret is refused; a resolved document, its debug form and its encoding hold only `(sensitive)`; the secrets are wiped when their holder is dropped |
| `meridian-cli/tests/secrets.rs` | through the real command: the value is in nothing `plan` and `apply` print and in no file of the state directory or of `compile --out`; a failed fetch repeats neither the value, a command's output nor its arguments; a secret read from the environment is not inherited by a program started afterwards; a secret that cannot be locked is warned about once, or refused with `--strict-secrets`; `secret check` reads no value and runs no command |
