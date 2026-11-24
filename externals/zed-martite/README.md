# Martite for Zed

Zed language support for [Martite](../../README.md): syntax highlighting,
bracket matching, auto-indentation, outline, text objects, comment toggling,
and CLI tasks for `.mt` source and `.mti` interface files.

The extension is grammar-based. It does not ship a language server, so inline
diagnostics, hover, go-to-definition, and format-on-save are not provided.

## Install for development

1. Install [Bun](https://bun.sh) 1.4.5 or newer.
2. From the repository root, install the grammar toolchain:

   ```sh
   cd externals/tree-sitter-martite
   bun install
   ```

3. In Zed, run `zed: install dev extension` and choose `externals/zed-martite`.
4. Open a `.mt` or `.mti` file.

Zed compiles the pinned tree-sitter grammar with `wasi-sdk`. Zed downloads it
automatically; if you already have it, set `WASI_SDK_PATH` to the directory
that contains `bin/clang`.

## What is included

| File | Purpose |
| --- | --- |
| `languages/martite/config.toml` | file suffixes, comments, tabs, brackets, overrides |
| `languages/martite/highlights.scm` | keywords, declarations, types, prelude names, literals, operators |
| `languages/martite/brackets.scm` | `()`, `[]`, and `{}` matching |
| `languages/martite/indents.scm` | blocks, list-style braces, match arms, `else` alignment |
| `languages/martite/outline.scm` | functions, handlers, structs, opaque structs, enums, variants, impls, consts, tests |
| `languages/martite/textobjects.scm` | function/class around and inside, comments |
| `languages/martite/runnables.scm` | `fn main` and `test "..."` run buttons |
| `languages/martite/tasks.json` | `martite check`, `lint`, `run`, `test`, `fmt` |
| `languages/martite/overrides.scm` | string and comment scopes |

All `.scm`, `config.toml`, and `tasks.json` files in `languages/martite/` are
generated. Do not edit them by hand; see below.

## CLI tasks

The extension ships these project tasks automatically:

```json
[
  { "label": "martite: check", "command": "martite", "args": ["check", "$ZED_FILE"] },
  { "label": "martite: lint", "command": "martite", "args": ["lint", "$ZED_FILE"] },
  { "label": "martite: run", "command": "martite", "args": ["run", "$ZED_FILE"], "tags": ["martite-main"] },
  { "label": "martite: test", "command": "martite", "args": ["test", "$ZED_FILE"], "tags": ["martite-test"] },
  { "label": "martite: fmt", "command": "martite", "args": ["fmt", "$ZED_FILE"] }
]
```

`fn main` and `test "..."` get run buttons because the runnables query tags them
and the matching tasks use those tags. To customize commands, add a
`.zed/tasks.json` in your project; project tasks take precedence over
language-provided tasks.

The repository includes its own `.zed/tasks.json` that runs `cargo run -q -p
martite-cli --` instead of assuming a globally installed `martite` binary.

## Generated queries

The grammar and all editor queries come from one TypeScript source of truth:

```sh
cd externals/tree-sitter-martite
bun install
bun run generate
```

- `grammar.ts` is the typed tree-sitter DSL; `scripts/generate-grammar.ts`
  emits the checked-in `grammar.js`.
- `query-spec.ts` holds the semantic metadata (keywords, prelude names,
  runnable tags, tasks) that the grammar cannot express.
- `scripts/generate-queries.ts` reads `src/grammar.json` and
  `src/node-types.json` and emits:
  - canonical `queries/highlights.scm`, `locals.scm`, and `tags.scm`
  - every Zed file under `languages/martite/`
  - the language task list

Generation fails when a keyword, node, field, or capture is unknown, when a
named node is neither covered nor explicitly ignored, or when a runnable tag
has no task. `bun run check` verifies that generated files are current and runs
the corpus, fixture, and query validation.

## Grammar pin

`extension.toml` pins the grammar to a commit in this repository:

```toml
[grammars.martite]
repository = "https://github.com/Brunzsn/Martite"
rev = "b51ac50ce254f40bcee48a24be1375863f8984f7"
path = "externals/tree-sitter-martite"
```

After changing the grammar, commit and push it, then update `rev` to the new
commit. Zed checks out that revision, so it will not see uncommitted changes or
a local working tree.

The extension currently lives inside the Martite monorepo for development. To
publish it in Zed's extension registry, extract this directory into a
standalone repository whose root contains `extension.toml`, and point
`repository` at that repository (the grammar `repository`/`path` can still
point at the Martite monorepo).

## Verification

Zed only supports installing dev extensions through the GUI; there is no
headless CLI command for it. After installing, check:

- `.mt` and `.mti` files select the Martite language.
- Keywords, types, prelude names, comments, strings, and interpolation
  highlight.
- Braces, brackets, and parentheses match and colorize.
- New lines indent inside blocks and list-style braces; `else` aligns with `if`.
- The outline lists declarations, variants, and tests.
- `]m` / `[m` and `af` / `if` navigate functions and classes in Vim mode.
- The run buttons appear on `fn main` and `test "..."`.
- `// ` toggles comments.
