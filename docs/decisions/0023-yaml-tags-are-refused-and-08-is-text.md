# YAML tags other than `!!str` are refused, and `08` is text

| | |
|---|---|
| Area | Language |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | `crates/meridian-ir/src/node.rs` (`check_tag`), `crates/meridian-yaml/src/scalar.rs` (`is_leading_zero_digits`) |
| Where it shows | [YAML pitfalls](../reference/dsl/yaml-pitfalls.md), the typing table on the [DSL reference](../reference/dsl/index.md#files) |

## Context

Kubernetes' KYAML work (KEP-5295) lists the places where YAML reads something other than what the author wrote. Checking Meridian against that list (issue 168) found two it had kept from the Go YAML library the first targets were written with: a tag other than `!!str` was silently ignored, so `!!int "12"` was the text `12` and `!!float 1` the integer 1; and `08` and `09`, which are not octal, were read as the floats 8 and 9 because the float rule accepted what the octal rule had refused.

## The choice

A document may carry the `!!str` tag and no other. `!!int`, `!!float`, `!!bool`, `!!null`, `!!binary`, `!!set`, `!!seq`, `!!map` and every local tag (`!custom`) are refused with the tag's line and column and a message saying to write the value plainly or quote it. Honouring the core-schema tags was the alternative; refusing them is less to document and less to get wrong, and no example or test in the repository used one.

A run of digits with a leading zero that is not valid octal (`08`, `09`, `0_9`, `-08`) is text. `08.5` is still the float 8.5, as the YAML 1.2 float rule says. On output the writer still quotes `"08"`, because the YAML 1.1 readers the tools use would take it for the number 8.

## What was checked and left alone

KEP-5295 states that go-yaml reads `_42` and `__4__2__` as numbers. Checked against gopkg.in/yaml.v2 2.4.0, gopkg.in/yaml.v3 3.0.1 and sigs.k8s.io/yaml 1.4.0 (the library kubectl reads with): all three read them as strings. Meridian's reader does too, and its writer leaves them unquoted; nothing needed to change.
