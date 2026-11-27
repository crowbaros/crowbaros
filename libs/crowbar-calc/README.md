# crowbar-calc

A dependency-free, pure Rust expression evaluator shared by Calculator and
the shell launcher. `evaluate(text, Mode::Real)` returns finite binary64
results; `Mode::Word(8 | 16 | 32 | 64)` returns exact unsigned words.
`Value::bases` formats all four programmer representations without converting
through floating point. Errors are typed and have person-facing messages.

`launcher_result` returns `None` for ordinary app queries, or the evaluated
math result/error. A leading `=` opts into math explicitly. Expressions are
limited to 4,096 bytes, 512 tokens, and 64 nested parsing calls; evaluating
input never runs code, reads files, or invokes a command.

The [Calculator guide](../../apps/calc/README.md) documents notation,
precedence, rounding, word widths, and overflow behavior. Run
`cargo test --locked` to test the grammar and both arithmetic modes.
