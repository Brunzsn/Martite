# External integrations

Editor and tooling integrations that consume the Martite compiler live here.
They are kept out of the Cargo workspace so they cannot become dependencies of
the language implementation.

```text
externals/
  tree-sitter-martite/   grammar.ts, generated parser, canonical queries, corpus
  zed-martite/           Zed extension, generated language queries and tasks
```

`tree-sitter-martite/grammar.ts` is the source of truth for the grammar. The
same package's `query-spec.ts` and generator emit both the canonical
tree-sitter queries and the Zed extension's query files, so highlighting cannot
drift between the two.

The Zed extension references the grammar through the monorepo `repository`
and `path` fields in `extension.toml`, so it does not copy generated parser
files.
