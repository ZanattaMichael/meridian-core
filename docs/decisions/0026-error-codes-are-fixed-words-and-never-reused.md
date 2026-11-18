# An error's kind is a fixed code that is never reused

| | |
|---|---|
| Area | Core |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | issue 100, typed errors across the core crates |
| Where it shows | [Error kinds and codes](../reference/errors.md) |

## Context

The core crates returned `Result<_, String>` from most of what can fail: an
expression that does not parse, a module that cannot be fetched, a stage whose
tool said no. A string is enough for the terminal, but a caller cannot act on
it without matching its words, and the words of a message are changed freely
to make it clearer. The orchestrator already had the fixed words a stage fails
with (`tool-failed`, `policy-denied`) as string constants, and a rollback
decided whether a step had reached its tool by comparing against two of them.

## The choice

Each crate's error is a struct with a `kind` and a message. The kind is an
enum with a fixed code (`expr-unknown-name`, `module-registry`,
`rules-file`, `signing-key`), and the stage reasons are the enum
`Reason` whose words are the ones the record already kept. The message is
unchanged, so the terminal reads as before, and every error still converts
into a `String`, so a caller that only prints it is not disturbed.

A code is part of the interface: it never changes meaning and is never
reused. A new sort of failure gets a new code, and a code whose failure stops
being produced stays reserved. Every code is listed on one reference page,
and a test fails when one is missing from it. Expression errors also carry
the span of the text they point at, so an editor can underline it.

The kinds are coarse on purpose: a kind says what sort of thing went wrong
and who can fix it, not which sentence was printed. A module crate error has
ten kinds, one per step of handling modules, not one per message.

## Alternatives

Keeping strings and documenting the messages. The messages are many and are
meant to change as they are made clearer; a list of them would be out of date
at once, and a caller matching on words would break with each improvement.

One error type for the whole workspace. The crates are used on their own
(the resolver, the signing crate), and a kind that means something in one has
no meaning in another; one type would carry every crate's kinds to every
caller.

Numbered codes (`E0042`). A word says what it is without a lookup, and the
record and the reports already kept words.
