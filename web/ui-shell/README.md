# UI Shell

Vite + TypeScript frontend for the Pera UI session server.

## Development

Install dependencies:

```bash
npm install
```

Start the UI shell:

```bash
npm run dev
```

Start the backend server in another terminal:

```bash
cargo run -p pera-cli -- serve --addr 127.0.0.1:3000
```

Then open the Vite URL, load `examples/ui_demo/specs/todo-app.json`, create a session, and interact with the rendered UI.
