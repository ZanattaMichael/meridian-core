# Secrets

A secret is a value a document needs and must not contain: a password, a
token, a private key. A document says **where a secret comes from**, by name,
and reads it by that name. The value is fetched when a run needs it, held in
protected memory, and never written anywhere Meridian writes.

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: site
spec:
  target: ansible
  targetHost:
    from: inline
    host: web01
  secrets:
    - name: db_password
      from: env
      key: SITE_DB_PASSWORD
    - name: deploy_token
      from: file
      path: /run/secrets/deploy-token
      optional: true
  resources:
    - id: token-login
      type: file
      when: secret.deploy_token != null       # only where a token was provided
      params:
        path: /etc/site/login-enabled
        content: "yes\n"
```

How the value is held in memory, and what is and is not promised about it, is
on [its own page](../../concepts/secrets-in-memory.md).

> **Where this stands.** A secret can be declared, fetched and read by a
> `when` today. A secret placed in a resource's `params` is checked, and
> `compile` writes its name in its place, but `plan` and `apply` stop at it:
> handing a secret to a tool needs a channel that is not the compiled files
> (the tool's standard input, or a file only the user can read), and each
> target gains its channel in a later release. A run never writes a secret
> into a compiled file to get past this.

## Contents

- [Declaring a secret](#declaring-a-secret)
- [The providers](#the-providers)
- [Reading a secret](#reading-a-secret)
- [What an expression can do with one](#what-an-expression-can-do-with-one)
- [Where a secret cannot be used](#where-a-secret-cannot-be-used)
- [When a secret is fetched](#when-a-secret-is-fetched)
- [What each command does](#what-each-command-does)
- [Errors](#errors)
- [Examples](#examples)

## Declaring a secret

`spec.secrets` is a list, in either kind of document. Each entry is one secret.

| Field | Required | Meaning |
|---|---|---|
| `name` | yes | the name it is read by, as `secret.<name>`: letters, digits and underscores, not starting with a digit |
| `from` | yes | the provider: `env`, `file` or `exec` |
| `key` | for `env` | the environment variable |
| `path` | for `file` | the file |
| `command` | for `exec` | the program and its arguments, as a list |
| `optional` | no | `true`: a secret that is not found is `null` instead of an error |
| `allowWideMode` | no, `file` only | `true`: read the file even though others than its owner can read it |
| `keepTrailingNewline` | no, `file` only | `true`: the file's last line ending is part of the secret |

There is no field for a value, and one written under any other name is refused
as an unknown field without being repeated in the error.

Where a secret comes from is written out. `key`, `path` and `command` cannot
hold an [interpolation](interpolation.md), so that `validate` can read every
declaration without any data.

A field that means nothing to the provider is refused (`path` on `from: env`,
for example), so a declaration never silently ignores part of what was written.

## The providers

| `from` | Reads | Refuses | Notes |
|---|---|---|---|
| `env` | the environment variable `key` | a variable that is not set (an empty one is an empty secret, not a missing one) | the variable is **removed from Meridian's environment** as soon as it is read, so no plugin or tool started afterwards inherits it. It is the weakest provider: the environment a process started with can be read by the same user while the process lives |
| `file` | the file at `path`, without its last line ending | a file that does not exist; a file its group or others can read or write, unless `allowWideMode: true`; a file larger than 256 KiB; a directory | a relative `path` is relative to where `meridian` is run |
| `exec` | what `command` prints, without its last line ending | a command that is not found, exits with a failure, is stopped by a signal, or prints nothing | the command is run **without a shell**: `command: [pass, show, site/db]` runs `pass` with two arguments, and nothing in them is expanded. What the command writes to its standard error is discarded. This is how a password manager or a cloud CLI is used |

The operating system's own stores (the macOS keychain, the Windows credential
manager, the Secret Service on Linux) and remote stores such as Vault are
planned as providers; until then, `exec` with the store's own command line tool
reaches them.

## Reading a secret

A secret is read as `secret.<name>` in an [expression](expressions.md): in a
`when`, a [named condition](interpolation.md#named-conditions), a
[variable](interpolation.md#variables), or an interpolation in a resource's
`params`.

```yaml
spec:
  secrets:
    - name: db_password
      from: env
      key: SITE_DB_PASSWORD
  conditions:
    password_is_default: secret.db_password == 'changeme'
  resources:
    - id: warn
      type: file
      when: cond.password_is_default
      params:
        path: /etc/motd
        content: "This host still has the default database password.\n"
```

A name that is not declared is refused by `validate`, with the names that are:

```
site.yaml:14:7: spec.resources[0].when: secret.db_pasword is not declared (spec.secrets declares: db_password)
```

What an expression holds when it reads a secret is not the secret. It is a
reference to it: which secret of the run is meant. The value stays in protected
memory, and everything made from the expression's result (a plan, a log line,
an error, the run's state) can only ever show `(sensitive)`.

## What an expression can do with one

| With a secret you can | The result is |
|---|---|
| compare it with `==` or `!=`, to text or to another secret | a plain `true` or `false`. The comparison takes the same time wherever the two differ |
| compare it with `null` (an optional secret that was not found is `null`) | a plain `true` or `false` |
| join it to text or another secret with `+` | a secret |
| place it inside text, as in `"password=${{ secret.db_password }}"` | the whole text is a secret |
| give it to `urlEncode`, `base64Encode`, `base64Decode` or `trim` | a secret |
| choose it with `? :`, `??`, `default`, `coalesce` or `required` | whichever value was chosen |
| test it with `defined` | a plain `true` or `false` |

Everything else refuses a secret, and says what would have worked:

```
upper(...) cannot be given a secret; the functions that can are base64Decode, base64Encode, trim, urlEncode, and a secret can be joined to text with + or placed inside text
```

That covers every other function (`length`, `sha256`, `toString`, `toJson`,
`replace`, `split`, `contains`, `regexMatch` and the rest) and every other
operator (`<`, `matches`, `in`, arithmetic). The list is short on purpose: each
entry does its work on protected memory, and a function that would have to
copy the value out to work on it is not on the list. The length of a secret
and a hash of it are refused because both say something about the value.

A secret mark never comes off. There is no function that turns a secret back
into plain text.

## Where a secret cannot be used

A secret is refused anywhere a value is shown, stored, or decides the shape of
the document:

| Place | Why |
|---|---|
| `metadata.labels` | labels are printed and matched by selectors |
| `spec.target`, `spec.targetVersion` | they pick the plugin |
| `spec.targetHost.host` | it is printed with every stage |
| a resource's `state` | it is part of the plan |
| an output's `value` | outputs are stored in the run's state and handed to other documents |
| `id`, `type`, a mapping key, an edge's resource name | the document's skeleton takes no interpolation at all |

```
site.yaml:1:1: metadata.labels.owner: a secret cannot be used here: this field is shown, stored or shapes the document. A secret can be read by a when, and placed in a resource's params
```

`validate` finds a secret written directly in one of these places. One that
gets there through a variable is found when the document is resolved, with the
same message.

## When a secret is fetched

- **Only `plan` and `apply` fetch.** `validate` and `compile` never read a
  provider, so they can run on a machine that has no access to the secrets.
- **Just before the stage that declares it.** A workflow of several documents
  fetches each document's secrets as its stage starts, not all of them up
  front.
- **Wiped when the stage ends.** The memory is overwritten as soon as the
  stage that declared the secret has finished, whether it succeeded or failed,
  not at the end of the run.
- **Not for a document that is left out.** A document whose `spec.when` is
  false fetches nothing, as long as that `when` does not itself read a secret.
  Neither does a stage that is not run because an earlier one failed.
- **Once, even with a policy.** `apply --policy` looks at every stage before
  it applies the first, and that first pass fetches nothing. A document that
  reads secrets is put to the [policy](../policy.md#when-a-policy-is-asked)
  at its own turn instead.
- **Not for a document that is only imported from.** A document compiled
  because another [imports](outputs-and-hand-offs.md#imports) a file from it is read
  as `compile` reads it: nothing is fetched, and a secret in its params is
  written as its name.
- **`drift` fetches as `plan` does**, because it plans each stage. `rollback`
  fetches nothing: it puts back artifacts that were kept.
- **Never cached.** Nothing is written to save a second fetch. The next run
  fetches again.

## What each command does

| Command | A `when` that reads a secret | A secret in `params` |
|---|---|---|
| `validate` | checked: the name is declared, the expression is one a secret allows where it can be known | checked the same way |
| `compile` | refused: `(secret db_password) is not fetched here: a secret is fetched by plan and apply, never by validate or compile` | written as its name: `password=(secret db_password)` |
| `plan`, `apply` | decided. The report shows the condition as the document wrote it, never the value | the stage stops: `resource "conf": params.content holds a secret, and target "ansible" cannot be handed a secret yet` (see [where this stands](#secrets)) |

What `compile --out` writes for a secret is a placeholder for reading, not
something a tool can run: `password=(secret db_password)`. A secret that went
through a function is written with the function around it:
`postgres://app:urlEncode((secret db_password))@db/app`.

## Errors

No error holds any part of a secret's value, or of what a command printed, or
a command's arguments.

| What is wrong | Caught by | Message |
|---|---|---|
| No name | `validate` | `spec.secrets[0].name: a secret needs a name; it is read as secret.<name>` |
| A name that cannot follow `secret.` | `validate` | `"has-dash" is not a usable name: a secret is read as secret.<name>, so its name is letters, digits and underscores, and does not start with a digit` |
| The same name twice | `validate` | `spec.secrets[1].name: the secret "a" is declared twice` |
| No provider, or one this version does not have | `validate` | `"vault" is not a provider this version has; from is one of env, exec, file` |
| The provider's field is missing | `validate` | `spec.secrets[0].key: from: env needs key, the environment variable to read` |
| A field the provider does not use | `validate` | `spec.secrets[0].path: path means nothing to from: env; remove it` |
| An interpolation in a declaration | `validate` | `where a secret comes from is written out, so that it can be read without any data; it cannot be interpolated` |
| A secret that is read and not declared | `validate` | `secret.x is not declared (spec.secrets declares none)` |
| A secret in a place that is shown or stored | `validate`, or when resolved | `a secret cannot be used here: ...` |
| A function or operator that does not take a secret | `plan`, `apply` | `length(...) cannot be given a secret; the functions that can are ...` |
| The environment variable is not set | `plan`, `apply` | `spec.secrets: secret "db_password" (from: env): the environment variable SITE_DB_PASSWORD is not set` |
| The file can be read by others | `plan`, `apply` | `secret "token" (from: file): ... has mode 0644 ... allowWideMode: true` |
| The command failed | `plan`, `apply` | `secret "flag" (from: exec): the command sh exited with status 4` |
| A secret that is not base64 is given to `base64Decode` | `plan`, `apply` | `base64Decode(...): the secret is not base64` |

## Examples

Under [`examples/language/`](../../../examples/language), each with the data
file it reads beside it:

| Example | Shows |
|---|---|
| [`features-by-secret.yaml`](../../../examples/language/features-by-secret.yaml) | three secrets, one from each provider, read only by conditions: an optional licence, a default password that is warned about, a secret beside ordinary data. It can be planned and applied today |
| [`secret-in-a-setting.yaml`](../../../examples/language/secret-in-a-setting.yaml) | a password placed in a database URL through a variable and `urlEncode`, and what `compile` writes in its place |

The first is planned with its password in the environment:

```
SITE_DB_PASSWORD=... meridian plan examples/language/features-by-secret.yaml \
    --data examples/language/features-by-secret.data.yaml
```

On a machine with no licence file and a password that is not the default, the
plan reports each condition in the document's own words and no value:

```
site (ResourceSet, target ansible on web01.example.com)
  when: 3 removed
    resource "premium_conf": cond.licensed is false
    resource "password_warning": cond.default_password is false
    "deploy_conf" depends on "premium_conf": cond.licensed is false
```

What each machine is given:

| The machine has | `premium_conf` | `password_warning` | `deploy_conf` | `hardening` |
|---|---|---|---|---|
| every secret, in production | kept | removed | kept | kept |
| no licence | removed | removed | kept | kept |
| no deploy token | kept | removed | removed | kept |
| the default password | kept | kept | removed | removed |
| a strong password outside production | removed | removed | removed | removed |

The second is compiled, which reads no secret:

```
meridian compile examples/language/secret-in-a-setting.yaml \
    --data examples/language/secret-in-a-setting.data.yaml --out out
```

```
DATABASE_URL=postgres://app:urlEncode((secret db_password))@db.internal:5432/app
SIGNING_KEY=base64Encode((secret signing_key))
LOG_LEVEL=info
```

Both are held in place by tests: the first is planned and applied for each row
of the table above and everything the runs print and leave behind is searched
for the secrets; the second is compiled with a password in the environment
that must not appear anywhere.
