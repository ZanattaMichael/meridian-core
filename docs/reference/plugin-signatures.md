# Plugin signatures

A plugin is a program that `meridian` starts with your own rights. Before it
starts one, `meridian` can check that the binary is the one somebody you trust
signed, and refuse to start it otherwise.

A released `meridian` does this by default, for the plugins it was released
with, and you need to do nothing. This page is for when you want to know what
is checked, use a plugin from somewhere else, or sign plugins of your own.

## What is checked

A signed plugin has a file beside it with the same name plus `.sig`:

```
meridian-target-terraform
meridian-target-terraform.sig
```

The `.sig` file holds an Ed25519 signature over the SHA-256 of the binary, and
the id of the key that made it. Before starting the plugin, `meridian`

1. reads the binary and computes its SHA-256,
2. looks for the signature's key among the public keys it trusts,
3. checks the signature against that key and that digest.

The plugin is started only after that. The check needs no network and no
service, so it happens every time, on every machine.

What it proves is exact and narrow: the binary is, byte for byte, the file the
holder of a trusted key signed. It does not say the plugin is good, only whose
it is. Which keys to trust is your decision.

## The policy

`--plugin-signatures`, or `MERIDIAN_PLUGIN_SIGNATURES`, says what to do:

| Policy | A plugin with a signature from a trusted key | Any other plugin |
|---|---|---|
| `require` | started | not started, and the command fails saying why |
| `warn` | started | started, with a warning on stderr |
| `off` | started | started; signatures are not looked at |

The default is `require` in a released `meridian` and `off` in one built from
source, which has no key of its own and whose plugins nobody signed.

`meridian plugins` shows what the check found for each plugin:

```
terraform
  ...
  binary:    /home/me/.meridian/bin/meridian-target-terraform
  signature: verified, signed by key 780001b5de1d7691
```

The other values are `unsigned`, `invalid: <why>` and `not checked`.

## Which keys are trusted

**The release's own key.** Each release build makes a new key pair, signs its
plugins, compiles the public key into its `meridian`, and deletes the secret
key before the build ends. So a released `meridian` trusts exactly the plugins
that were built with it, and no signing secret exists afterwards to be stolen.
The public key is also in the archive, as `release-key.pub`.

The consequence: plugins and `meridian` from one release go together. A
plugin copied from another release, or another platform's archive, was signed
with another key and is refused. Install a release whole.

How you know the `meridian` binary itself is genuine is a separate check, made
when you install it: [Check what you downloaded](../quickstart/install.md#check-what-you-downloaded).

**Keys you name.** `--trusted-key FILE` trusts one more public key, and can be
given more than once. A directory means every `*.pub` file in it.
`MERIDIAN_TRUSTED_KEYS` is the same as a list, separated by `:` (`;` on
Windows).

```
meridian plugins --trusted-key ~/keys/acme.pub
export MERIDIAN_TRUSTED_KEYS=/etc/meridian/trusted-keys
```

## Using a plugin from somewhere else

A third-party plugin, or one you built yourself, is not signed by the
release's key. Under `require` it is not started:

```
error: plugin /opt/plugins/meridian-target-acme: the plugin was not started: it is not signed: ...
```

Choose one:

- Get the author's public key from somewhere you trust, and pass it with
  `--trusted-key`. This is the one to prefer.
- Sign the plugin with a key of your own, as below, and trust that key.
- Run with `--plugin-signatures warn`, which starts it and says so each time.

Keep such a plugin in a directory of its own and name it with `--plugin-dir`;
the install scripts replace the plugins in the directory they install into.

## Signing plugins of your own

Make a key pair once:

```
meridian signing keygen --secret acme.key --public acme.pub
```

`acme.key` signs; keep it private. It is written so that only you can read it,
and an existing key is never written over. `acme.pub` is what the people who
run your plugin trust; publish it somewhere they have reason to believe is
yours.

Sign each binary you ship, after the last thing that changes it (stripping,
compressing, code-signing):

```
meridian signing sign --key acme.key meridian-target-acme
```

That writes `meridian-target-acme.sig`. Ship the two files together. Sign each
platform's binary separately: they are different files.

Check a binary without starting it:

```
meridian signing verify --trusted-key acme.pub meridian-target-acme
meridian-target-acme: verified, signed by key 5f1c0a94e2b7d310
```

It exits non-zero if any binary does not verify.

## File formats

All three are short text files.

| File | Content |
|---|---|
| public key | `meridian-public-key-v1 <64 hexadecimal digits>`; anything after the key on the line is a comment |
| secret key | `meridian-secret-key-v1 <64 hexadecimal digits>` |
| signature | four lines: `meridian-plugin-signature-v1`, `key <id>`, `sha256 <digest>`, `sig <128 hexadecimal digits>` |

A key's id is the first 16 hexadecimal digits of the SHA-256 of the public
key. It names a key in messages and signature files; it is not itself trusted.

The signed message is the line `meridian-plugin-signature-v1`, a newline, the
binary's SHA-256 in lower-case hexadecimal, and a newline. The `sha256` line in
the signature file is there to give a clearer message when a binary has
changed; the check uses the digest of the file as it is.

## What this does not cover

- Whoever can replace both `meridian` and a plugin can replace the key too.
  The signature protects a plugin directory from being tampered with on its
  own; the install-time checks protect the installation as a whole.
- The check is made when the plugin is started. A binary replaced between the
  check and the start, by someone who can already write to your plugin
  directory, is not caught.
- A target compiled into `meridian` itself is part of that binary and has no
  signature of its own.

## Using the check from other code

The signing and checking code is the `meridian-signing` crate
(`crates/meridian-signing`). It is not about plugins: `SecretKey::sign_file`
signs any file, `verify_file` checks a file and its `.sig` against a list of
`PublicKey`s, and `Verification::check` applies a `Policy` (off, warn or
require) to the result. `SecretKey::sign_bytes` and `verify_bytes` do the same
for bytes that are not a file, which is how
[modules are signed](modules.md#signatures): with the same keys, trusted
through the same `--trusted-key`. Such a signature starts with
`meridian-statement-signature-v1`, and that name is part of what is signed, so
a signature over a statement never passes as a plugin's and a plugin's never
passes as a statement's. The plugin host re-exports it as
`meridian_plugin_host::signature`.
