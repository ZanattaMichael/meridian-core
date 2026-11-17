## What this changes

<!-- One or two sentences. Link the issue it closes, if there is one: Closes #123 -->

## How it was tested

<!-- What you ran, and against what. Say plainly if a runner has only met a
     stand-in and not the real tool. -->

## Checklist

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked`
- [ ] `python3 tools/check_links.py`
- [ ] A test covers the change; a bug fix has a test that fails without it
- [ ] Golden files were recorded on purpose and their diff was read
- [ ] The documentation is updated in this pull request
- [ ] A design choice made in this change has a record under `docs/decisions/`

## For a new or changed plugin

<!-- Delete this section if it does not apply. The full list is in
     docs/contributing/writing-a-plugin.md. -->

- [ ] Everything the tool cannot express is refused with a reason, and each refusal has a test
- [ ] The declared capabilities, category and resource types claim only what a test proves
- [ ] `docs/targets/<name>.md` opens with what is and is not supported
- [ ] The target is listed in the README, the docs index, the install page and the capability matrix

## Needs a second reviewer

<!-- Tick any that apply; see CONTRIBUTING.md. -->

- [ ] Builds or escapes a shell command string
- [ ] Handles a secret or a sensitive output
- [ ] Changes what is written to the run-state
- [ ] Changes the plugin protocol
