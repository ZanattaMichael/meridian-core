# The MCP model test set

How we know [the MCP server](../../docs/tooling/mcp-server.md) works: a small
set of tasks in plain words, each answered by a model with and without the
server, and scored by the compiler.

- `tasks.json`: the tasks. Each has an `id`, the `target` and document
  `kind` it is for, the `task` in a sentence or two, the resource types a
  correct answer uses (`expectTypes`), and words the answer must contain
  (`expectText`), such as `notifies` for a task that asks for a reload.
- `check.py`: scores a directory of answers through `meridian mcp` itself,
  with no model needed.

## Running it

1. Build `meridian` and its plugins (`cargo build --workspace`), and put
   `target/debug` on the `PATH` or pass `--meridian target/debug/meridian`
   with `MERIDIAN_PLUGIN_DIR=target/debug`.
2. For each model and each condition (with the server attached, without),
   give the model each task's `task` text and save its final document as
   `<run>/<task id>.yaml`. With the server, attach the `authoring-guide`
   prompt and let the model call the tools; record how many tool calls or
   turns it took. Without, give it only the task.
3. Score each run:

   ```
   python3 tools/mcp_eval/check.py --answers runs/small-with-server
   python3 tools/mcp_eval/check.py --answers runs/small-without-server
   ```

4. Record on the documentation page, per model and condition: the share
   that compile, the share that use the expected types and say what was
   asked, and the number of turns. The server is worth shipping when the
   small model with it does clearly better than the same model without.

This runs on demand, not in CI, since it needs model access. The checks
themselves are run by `crates/meridian-cli/tests/mcp_eval.rs`, which scores
the examples as answers so that the scorer cannot rot.
