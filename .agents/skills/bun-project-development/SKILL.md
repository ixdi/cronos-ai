---
name: bun-project-development
description: Follow this project's Bun-first conventions when implementing, testing, running, or building JavaScript and TypeScript code. Use when working in this repository on scripts, tests, servers, databases, or frontend assets.
---

# Bun Project Development

Follow the repository's Bun-first conventions for JavaScript and TypeScript work.

## Runtime and commands

- Prefer Bun over Node.js and its ecosystem equivalents.
- Run source files with `bun <file>` rather than `node <file>` or `ts-node <file>`.
- Run tests with `bun test` rather than Jest or Vitest.
- Build HTML, TypeScript, and CSS with `bun build <file.html|file.ts|file.css>` rather than Webpack or esbuild.
- Install dependencies with `bun install` rather than npm, Yarn, or pnpm.
- Run package scripts with `bun run <script>` rather than the corresponding npm, Yarn, or pnpm command.
- Run package binaries with `bunx <package> <command>` rather than npx.
- Bun loads `.env` automatically, so do not add dotenv just to load environment variables.

## Prefer Bun APIs

- Use `Bun.serve()` for HTTP servers, routes, HTTPS, and WebSockets. Do not introduce Express.
- Use `bun:sqlite` for SQLite instead of `better-sqlite3`.
- Use `Bun.redis` for Redis instead of `ioredis`.
- Use `Bun.sql` for PostgreSQL instead of `pg` or `postgres.js`.
- Use the built-in `WebSocket` instead of the `ws` package.
- Prefer `Bun.file` over `node:fs` `readFile` and `writeFile` for file access.
- Prefer `Bun.$` for shell commands instead of adding execa.
- Before adding a dependency or using a Node-specific API, check whether Bun already provides the needed capability.

## Testing

Write and run tests with Bun's test runner. Import test helpers from `bun:test`:

```ts
import { expect, test } from "bun:test";

test("hello world", () => {
  expect(1).toBe(1);
});
```

Run the relevant tests with `bun test` and include broader checks when appropriate.

## Frontend and server development

For frontend work, prefer Bun's native HTML imports with `Bun.serve()` over introducing Vite. Bun can bundle imported TSX, JSX, and JavaScript from HTML, and CSS can be included through stylesheet links or imports.

A basic server can serve an HTML entry point directly:

```ts
import index from "./index.html";

Bun.serve({
  routes: {
    "/": index,
    "/api/users/:id": {
      GET: (req) => Response.json({ id: req.params.id }),
    },
  },
  development: {
    hmr: true,
    console: true,
  },
});
```

An HTML entry point can import frontend code directly:

```html
<html>
  <body>
    <h1>Hello, world!</h1>
    <script type="module" src="./frontend.tsx"></script>
  </body>
</html>
```

Use `bun --hot ./index.ts` for a hot-reloading development server. Follow the existing project structure and dependencies if they establish a more specific convention.

## Bun API references

For further Bun API details, consult `node_modules/bun-types/docs/**.mdx` when available.
