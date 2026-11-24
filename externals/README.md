# External integrations

Editor and tooling integrations that consume the Martite compiler live here.
They are kept out of the Cargo workspace so they cannot become dependencies of
the language implementation.

```text
externals/
  tree-sitter-martite/   tree-sitter grammar, queries, and corpus tests
  zed-martite/           Zed extension (planned)
```

The tree-sitter grammar is the shared parser used by editor extensions. The
planned Zed extension can reference it directly through the repository `path`
field, without copying generated parser files.
