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
| tree-sitter CLI | `0.27.2` |
| Parser ABI | `15` |
| pnpm | `12.3.1` |

The CLI is pinned in `package.json` and `pnpm-lock.yaml`. Generated parser
files are checked in:

- `src/parser.c`
- `src/grammar.json`
- `src/node-types.json`
- `src/tree_sitter/*.h`

`src/scanner.c` is handwritten and is not generated.

## Commands

```sh
pnpm install
pnpm run generate       # regenerate src/ after editing grammar.js
pnpm test               # run the corpus tests
pnpm run check-fixtures # parse the repository's .mt/.mti fixtures
pnpm run check          # verify generated files + corpus + fixtures
```

`pnpm run check` uses `check-generated.mjs`, which runs a fresh generate in a
temporary directory and compares the result with `src/`. It does not require a
clean git worktree.

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

The grammar exposes these node names for highlighting and navigation:

- Declarations: `function_declaration`, `on_declaration`, `struct_declaration`,
  `opaque_struct_declaration`, `enum_declaration`, `variant`, `impl_declaration`,
  `const_declaration`, `test_declaration`, `use_declaration`, `field_declaration`
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

Starter queries live in `queries/highlights.scm`, `queries/locals.scm`, and
`queries/tags.scm`.

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
subdirectory:

```toml
[grammars.martite]
repository = "https://github.com/Brunzsn/Martite"
rev = "tree-sitter-martite-v0.1.0"
path = "externals/tree-sitter-martite"
```

The `rev` must be a tag or commit SHA that contains the generated `src/parser.c`.
Create the tag after the grammar commit is on the main branch:

```sh
git tag -a tree-sitter-martite-v0.1.0 -m "tree-sitter-martite 0.1.0"
git push origin tree-sitter-martite-v0.1.0
```

### Publishing as a standalone repository

Zed supports `path` for subdirectory grammars, so a separate repository is
optional. If you prefer one, extract this directory with:

```sh
git subtree split --prefix=externals/tree-sitter-martite -b tree-sitter-martite
```

then push that branch to a standalone `tree-sitter-martite` repository. The
generated `src/` files and `package.json`/`tree-sitter.json` are already
self-contained; no Cargo dependency is required.
