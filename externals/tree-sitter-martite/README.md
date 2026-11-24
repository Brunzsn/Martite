# tree-sitter-martite

Tree-sitter grammar for the Martite language. It parses both `.mt` source files
and `.mti` interface files with one grammar, so editors do not need a second
parser for interface mode.

The Rust lexer and parser in
[`../../crates/martite-syntax`](../../crates/martite-syntax) remain the
normative implementation. This grammar is for editors, syntax highlighting,
code navigation, and other tree-sitter tooling. It intentionally accepts the
union of source and interface syntax and leaves mode-specific checks to the
compiler.

## Toolchain

| Tool | Version |
| --- | --- |
| Bun | `1.4.5` |
| tree-sitter CLI | `0.27.2` |
| Parser ABI | `15` |

The CLI is pinned in `package.json` and `bun.lock`, and is downloaded by
`scripts/install-tree-sitter.ts` with Bun, so no Node installation is needed.
Generated files are checked in:

- `grammar.js` (generated from `grammar.ts`)
- `src/parser.c`
- `src/grammar.json`
- `src/node-types.json`
- `src/tree_sitter/*.h`
- `queries/*.scm`

`src/scanner.c` is handwritten and is not generated.

## Commands

```sh
bun install
bun run generate       # grammar.ts -> grammar.js -> src/ -> queries
bun test               # run the corpus tests
bun run check-fixtures # parse the repository's .mt/.mti fixtures
bun run check-queries  # validate every query against the fixtures
bun run check          # typecheck + generated-file drift + corpus + fixtures + queries
```

`bun run check` runs `scripts/generate.ts --check`, which verifies
`grammar.ts` -> `grammar.js`, compares a fresh parser generate with `src/`, and
regenerates the canonical and Zed query files in memory. It does not require a
clean git worktree.

The Zed extension lives in [`../zed-martite`](../zed-martite).

Zed has supported tree-sitter ABI 15 since tree-sitter 0.25, and has shipped
tree-sitter 0.26.9 since May 2026, so the pinned ABI is safe for the extension.

## Corpus

| File | Coverage |
| --- | --- |
| `test/corpus/lexical.txt` | keywords, identifier classes, numbers, strings, comments, doc comments, suppressions |
| `test/corpus/newlines.txt` | statement boundaries, continuation rules, list-style braces, non-associativity, match arm commas |
| `test/corpus/items.txt` | `use`, visibility, structs, enums, `impl`, `const`, `fn`, handlers, tests |
| `test/corpus/types.txt` | primitive and compound types, optionals, function types, generics and bounds |
| `test/corpus/expressions.txt` | precedence, chains, conversions, literals, struct literals, control flow, lambdas |
| `test/corpus/patterns.txt` | wildcard, literals, bindings, variants, structs, tuples, or-patterns |
| `test/corpus/statements.txt` | declarations, loops, jumps, assignments, newline-sensitive returns |
| `test/corpus/interpolation.txt` | nested expressions, map literals, nested strings, escapes, recovery |
| `test/corpus/interface.txt` | opaque structs, host functions, hooks, constants, newline-terminated signatures |
| `test/corpus/recovery.txt` | incomplete buffers and malformed items/statements |

The fixture checker parses every `.mt` and `.mti` under `examples/`, the
conformance fixtures, the formatter fixtures, the CLI fixtures, and the
deterministic fuzz corpus. The expected syntax-error fixtures are
`crates/martite-conformance/fixtures/bench/parser/malformed.mt`,
`fuzz/corpus/checker/syntax-error.mt`, and
`fuzz/corpus/checker/deep-nesting.mt` (which has an extra closing parenthesis).

## Stable node names

The grammar is written in `grammar.ts` and transpiled to `grammar.js`. The
`queries/*.scm` files are generated from `query-spec.ts` and the generated
`grammar.json`/`node-types.json`; edit those sources, not the query files.

The grammar exposes these node names for highlighting and navigation:

- Declarations: `function_declaration`, `on_declaration`, `struct_declaration`,
  `opaque_struct_declaration`, `enum_declaration`, `variant`, `impl_declaration`,
  `const_declaration`, `test_declaration`, `use_declaration`, `field_declaration`
- Braces: `block_lbrace`, `list_lbrace`, `map_lbrace`, `rbrace` are visible
  scanner tokens so editors can match and colorize `{}`
- Names and bindings: `identifier`, `type_identifier`, `constant`, `parameter`,
  `self_parameter`, `binding_pattern`, `field_pattern`, `type_parameter`, `bound`
- Types: `primitive_type`, `named_type`, `optional_type`, `tuple_type`,
  `function_type`, `list_type`, `map_type`, `type_arguments`
- Literals: `integer_literal`, `float_literal`, `boolean_literal`, `string`,
  `string_content`, `escape_sequence`, `interpolation`, `unit_expression`
- Expressions: `or_expression`, `and_expression`, `not_expression`,
  `comparison_expression`, `range_expression`, `additive_expression`,
  `multiplicative_expression`, `unary_expression`, `coalesce_expression`,
  `call_expression`, `field_expression`, `index_expression`, `try_expression`,
  `if_expression`, `match_expression`, `match_arm`, `lambda_expression`,
  `struct_literal`, `map_literal`, `list_literal`, `tuple_expression`
- Statements: `block`, `let_statement`, `var_statement`, `while_statement`,
  `for_statement`, `return_statement`, `break_statement`, `continue_statement`,
  `expression_statement`
- Patterns: `wildcard_pattern`, `literal_pattern`, `binding_pattern`,
  `variant_pattern`, `struct_pattern`, `tuple_pattern`, `or_pattern`,
  `rest_pattern`
- Trivia: `comment`, `doc_comment`

Canonical queries are generated into `queries/highlights.scm`,
`queries/locals.scm`, and `queries/tags.scm` by `scripts/generate-queries.ts`.

## Interface mode

Body-less `fn` and `on`, brace-less opaque `struct`, and host `const`
declarations are accepted by the same grammar. The grammar cannot see a file
extension, so a few forms are accepted in `.mt` files that the compiler rejects
in source mode. The Rust parser remains the authority for those mode checks.

The grammar also deliberately cannot enforce every parser restriction. It
accepts raw newlines inside strings for editor recovery, and it relies on the
compiler for identifier casing, host `mut` restrictions, and the E002 depth
limit.

## Zed manifest

If you use this repository directly, point the grammars section at the
subdirectory and a pushed commit or tag that contains `src/parser.c`:

```toml
[grammars.martite]
repository = "https://github.com/Brunzsn/Martite"
rev = "b51ac50ce254f40bcee48a24be1375863f8984f7"
path = "externals/tree-sitter-martite"
```

The tag `tree-sitter-martite-v0.1.0` is reserved for the first standalone
grammar release; create and push it after the release commit is on the main
branch, then update the extension pin.

### Publishing as a standalone repository

Zed supports `path` for subdirectory grammars, so a separate repository is
optional. If you prefer one, extract this directory with:

```sh
git subtree split --prefix=externals/tree-sitter-martite -b tree-sitter-martite
```

then push that branch to a standalone `tree-sitter-martite` repository. The
generated `src/` files, `grammar.js`, `queries/`, `package.json`,
`tree-sitter.json`, and `bun.lock` are self-contained; no Cargo dependency is
required.
