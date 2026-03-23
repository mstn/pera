# Examples

These Python examples are written to match the kind of code Monty is designed to handle in this project:

- plain expressions and top-level results
- basic control flow and collection operations
- `print(...)`
- comprehensions and dictionary/list processing
- `await` on external functions provided by the host runtime

Run them with:

```bash
cargo run -p pera-cli -- run examples/<file>.py --root .pera
```

Notes:

- examples that do not call external functions can complete successfully
- examples that use `await`ed external functions will currently fail in this repo, because the CLI still wires `RejectingActionHandler`
- those async examples are still useful because they show the code shape Monty supports and exercise suspend/resume behavior
