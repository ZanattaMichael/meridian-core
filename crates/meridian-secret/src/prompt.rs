//! Taking a secret from the person at the keyboard, or from a pipe.

use std::io::{IsTerminal, Read, Write};

use crate::{sys, SecretValue, DEFAULT_MAX_LEN};

/// Standard input, read without the standard library's buffer, which would
/// keep a copy of what passed through it in ordinary memory.
struct RawInput {
    /// Stop at the end of the first line, as a prompt does.
    one_line: bool,
    done: bool,
}

impl Read for RawInput {
    fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        if self.done || into.is_empty() {
            return Ok(0);
        }
        if !self.one_line {
            return sys::stdin_read(into);
        }
        // A byte at a time, so nothing after the line is taken with it.
        let read = sys::stdin_read(&mut into[..1])?;
        // A carriage return ends the line as well: some terminals send one.
        if read == 0 || into[0] == b'\n' || into[0] == b'\r' {
            self.done = true;
            return Ok(0);
        }
        Ok(read)
    }
}

/// Reads a secret from standard input into protected memory.
///
/// At a terminal, `prompt` is shown on standard error, what is typed is not
/// shown, and the secret is the line typed. From a pipe or a file the
/// secret is everything read, without its last line ending. Never from an
/// argument: arguments are visible to every user in the process list.
pub fn read_from_stdin(prompt: &str) -> std::io::Result<SecretValue> {
    if !std::io::stdin().is_terminal() {
        let input = RawInput {
            one_line: false,
            done: false,
        };
        return SecretValue::from_reader(input, DEFAULT_MAX_LEN)?.without_trailing_newline();
    }
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let was = sys::echo_off(0);
    let input = RawInput {
        one_line: true,
        done: false,
    };
    let read = SecretValue::from_reader(input, DEFAULT_MAX_LEN);
    if let Some(was) = &was {
        sys::echo_restore(0, was);
    }
    read
}
