# Martite: The Rules

*Status: spec-freeze candidate, language edition 1. Sections 0 to 10, 13, and 14 are normative. Section 11 (embedding API) is an illustrative sketch. Section 12 is normative for lint IDs, triggers, exemptions, tiers, primary spans, suppression, config resolution, and fix semantics; its severities, thresholds, and presets are recommended defaults that tools may tune.*

A design for a small, statically typed, interpreted language with a linter-friendly core. **Martite's primary job is to be embedded in an application as its plugin language:** the host app exposes an API, and plugins are Martite programs that run sandboxed against it. It remains a complete general-purpose language, though. The core language and standard library have no ambient I/O, and the standalone CLI is simply one more host that happens to grant file and console access. The reference implementation (interpreter, linter, and an embedding crate) is planned in Rust; this document covers design only.

A feature only gets in if it passes five tests: a beginner can learn it in a few minutes, the checker can decide it locally, a linter can see through it, a tree-walking interpreter in Rust can run it cheaply, and a host can bound what it costs and what it can reach.

## Design highlights

| Change | Reason |
| --- | --- |
| Casing is syntax: `snake_case` values, `PascalCase` types and variants, `SCREAMING_CASE` consts | Patterns become unambiguous (`Red` is a variant, `red` is a binding), and the naming lint moves into the lexer |
| Prelude variants are `Some`, `None`, `Ok`, `Err` | Same casing rule |
| Iteration uses `for (i, v) in x.enumerate()` | One general mechanism (tuples and patterns) instead of a special form |
| User generics and `impl` blocks added, with 3 built-in bounds and no traits | Real programs need `Stack[T]`, but trait solving is the expensive part |
| Tuples, lambdas, `mut` parameters, `pub`/`use`, `test` blocks added | Modern conveniences that are all locally checkable |
| Lists inside braces use commas, with a trailing comma allowed | Keeps the parser LL(1) when an arm starts with `-1` |
| 28 keywords | A compact, explicit keyword set (`on` added for plugin handlers, `as` for import aliases) |
| Plugin entry points are `on` handlers; `fn main` is only for standalone scripts | A plugin is a set of hooks the host calls, not a program that runs once |
| Plugin state is whatever `init` returns, and handlers receive it as `self` or `mut self` | Reuses copy-in/copy-out, so a trapped handler leaves state untouched for free |
| Host APIs are described in `.mti` interface files, and a plugin may only `use` modules its manifest grants | No ambient authority: the `use` lines are the plugin's complete reach |
| Panics become traps returned to the host, with host-set fuel, memory, and depth limits | Untrusted code can't hang or kill the app |
| Fuel is charged in proportion to work done, not just per node | A single built-in call such as `sorted` can't burn unbounded time for one unit of fuel |
| `std.io` and `std.env` leave the core and become standalone-host modules | The core is pure and deterministic |
| Language edition (`lang`) and interface version (`api`) are both declared in the manifest | Prelude and std growth can never silently break an existing plugin |
| `martite fmt` has one canonical style (section 14), and `// allow` covers the next syntactic element rather than the next line | Formatting can never reorder, wrap, or reflow code in a way that changes what the linter reports |
| `Eq`, `Ord`, and `Show` are structural, and the `Show` text format is specified | Interpolation, `print`, and sorting behave identically in every implementation |

## 0. Files, modules, editions, and load limits

- **File types:** `.mt` source, `.mti` host interface, `plugin.toml` plugin manifest, `martite.toml` tool configuration (lint presets, groups, severities, thresholds, options, formatter options).
- **Encoding:** UTF-8. A leading BOM is ignored. `\r\n` is normalized to `\n` before lexing. Tabs are legal whitespace. A NUL byte is a lexical error.
- **Modules:** a file is a module. The **project root** is the directory of the entry file (the manifest's `entry` for a plugin, the file given to `martite run` for a script). `use geometry.shapes` loads `<root>/geometry/shapes.mt`. `std.*` is built in. Host modules come from interface files (`app/editor.mti` is module `app.editor`). A module path that matches both a source file and an interface is E106.
- **The interpreter never touches the filesystem.** It asks a `SourceProvider` for the text of a module path. The CLI supplies a filesystem provider, and an embedding supplies whatever it likes (an in-memory map, an archive).
- **Provider policy in the reference implementation.** `martite run` uses an unrestricted filesystem provider. `martite check`, `martite test`, and embedded plugin hosts use a sandboxed provider rooted at the project directory by default; an embedding must explicitly opt in to unrestricted access. Sandboxing rejects absolute paths, drive prefixes, `.`/`..` components, empty components, and symlink escapes.
- **Editions.** The manifest declares `lang = "1"`. Within an edition, the keyword set, prelude names, standard library surface, `Show` format, fuel table (section 8), and error codes never change incompatibly. New keywords or prelude names appear only in a new edition, so existing code cannot start hitting E103 shadowing errors. `martite run` on a script uses the edition in `martite.toml`, or the newest one if absent.
- **Load limits** (host-configurable; defaults shown). They exist so that parsing and checking hostile input cannot exhaust the host's native stack or memory. Exceeding one is E002.
  - source file size: 1 MiB
  - syntactic nesting depth (expressions, patterns, types, interpolations, blocks): 128
  - total source files per plugin: 256

## 1. Lexical rules

- **Keywords (28):** `fn let var const struct enum impl use as pub test on if else match while for in return break continue mut self and or not true false`
- **Contextual names:** `init` and `shutdown` are ordinary identifiers except directly after `on`, where they name the lifecycle handlers (section 4).
- **Reserved type names (7):** `int float bool str list map range`
- **Prelude names (cannot be declared, per the no-shadowing rule):** `Option Result Some None Ok Err print panic assert`, plus the reserved type names, plus the bound names `Eq Ord Show`.
- **Identifier classes** (ASCII only):
  - `snake_case`: `_?[a-z][a-z0-9_]*`. Values, functions, fields, methods, modules, handlers. A leading `_` marks "intentionally unused" (L001). A lone `_` is the wildcard token, not an identifier.
  - `PascalCase`: starts with an uppercase letter, contains only letters and digits, and has at least one lowercase letter, or is a single capital (`T`). Types, variants, type parameters. `HTTPServer` is fine, `ID` is not (it is SCREAMING).
  - `SCREAMING_CASE`: `[A-Z][A-Z0-9_]+` (at least two characters). Consts only.
- **Literals:**
  - Ints: `42`, `1_000`, `0xFF`, `0b101`. Underscores go between digits only. No leading zeros (`007` is E001, to avoid octal confusion). The range is signed 64-bit. `9223372036854775808` is accepted only as the direct operand of unary `-`.
  - Floats need digits on both sides of the `.`, or an exponent: `2.0`, `0.5`, `1e9`, `1.5e-3`. `2.` and `.5` are not floats (`2.` lexes as the int `2` followed by `.`), which is also why `1..5` is a range.
  - After a `.` in postfix position the lexer reads a plain decimal integer, never a float, so `t.0.1` means `(t.0).1`.
  - Strings: `"…"` with escapes `\n \r \t \0 \\ \" \{ \} \u{HEX}`. No raw newlines. Any other escape is E001.
- **Interpolation:** `"{expr}"` where `expr` is any expression, converted with the `Show` format of section 9.2 (a `str` is inserted raw). The lexer tracks brace depth, so string literals and map literals may appear inside an interpolation. An empty `{}` is E001. An unescaped lone `}` is also E001, so write `\}` for a literal brace. Format specifications are not supported; use `x.round(2)`.
- **Comments:** `//` and `///`. A `///` directly before a *doc-able element* is its doc comment, and anywhere else it is an ordinary comment. Doc-able elements are top-level items, `impl` methods, struct fields, and enum variants; blank lines between the comment and the element do not detach it. Comments are kept as trivia on tokens so the linter and formatter can see them.
- **Suppression:** `// allow(L001)` or `// allow(L001, L002)` suppresses those lint IDs for diagnostics whose primary span starts within the head of the next syntactic element: a statement, a struct field, an enum variant, an `impl` method, a match arm, or an item. The head is the whole element for simple statements, fields, variants, `const`, and `use`, and everything through the opening `{` for other items, `impl` methods, `if`, `while`, `for`, `match`, and block lambdas (section 12.3). Compile errors (`E…`) cannot be suppressed.
- **Newlines** end statements. A newline is ignored when:
  - the previous token is a binary operator (including `|` in or-patterns), an assignment operator, `,`, `:`, `=>`, `->`, or an opening bracket;
  - the next token is `.` (method chains), `else`, `and`, `or`, or `??`, none of which can start a statement;
  - the innermost enclosing bracket is `()`, `[]`, or a **list-style brace** (struct, enum, impl, and match bodies, struct literals, map literals).

  The innermost bracket wins. A block `{ }` makes newlines significant again, even when it sits inside parentheses, which is what lets `xs.map(fn(x) { let y = x + 1 \n y })` work. Blank lines are insignificant, and `return` followed by a newline returns `()`.
- **Operators and punctuation:** `+ - * / % == != < <= > >= = += -= *= /= %= .. ..= ?? ? => -> . , : | _ ( ) [ ] { }`. `??` is lexed greedily, so `x??` must be written `(x?)?`.

## 2. Types

```
int          64-bit signed, overflow panics
float        IEEE 754 f64, never panics (inf and NaN exist)
bool, str    str is UTF-8, immutable value
()           unit, the type of "no value"
(T, U, …)    tuples, access by .0 .1, destructure by pattern; a 1-tuple is written (T,) and (e,)
list[T]      map[K, V]    range (0..n, 0..=n, ints only)
T?           sugar for Option[T]       Result[T, E]
fn(A, B) -> R
user types   struct S, enum E, both optionally generic: Pair[A, B]
```

- **`str`:** lengths and indices count Unicode scalar values (not bytes, not grapheme clusters). `==` compares scalar values with no normalization, and `<` is lexicographic by scalar value. Because strings are UTF-8 underneath, `len` and slicing are O(n) in general; implementations may keep an ASCII fast path.
- **`range`:** `a..b` is empty when `a >= b`, and `a..=b` is empty when `a > b`. A range is a value (`let r = 0..3`) and works for `for` and slicing.
- **`T?`** binds tighter than `->`, so `fn() -> int?` is a function returning `int?`. `T??` is not syntax (write `Option[T?]`), and an optional function type must be written `Option[fn() -> int]`.
- **Never** is an internal type for `return`, `break`, `continue`, `panic`, and a `while true` with no `break`. It unifies with anything and can't be written.
- **Equality:** user types are nominal. Built-ins, tuples, and function types are structural. There is **no subtyping and no coercion**, not even `int` to `float` or `T` to `T?`. Write `float(n)` and `Some(x)`.
- **Recursive types** are allowed (`enum Tree { Leaf, Node(Tree, int, Tree) }`). Sizing is the implementation's problem, not the user's.
- **Types are erased at runtime.** Generic code is not monomorphized, and `Eq`, `Ord`, and `Show` dispatch on the runtime value.
- **Data types.** A type is a *data type* if it contains no function type anywhere inside it. Only data types can be plugin state, cross the host boundary, or be snapshotted (E506).
- **Built-in bounds** for generics are `Eq`, `Ord`, and `Show`, written `[T: Ord + Show]`. They are automatic and structural, and cannot be implemented by users. Bounds may appear only on `fn` and `impl` type parameters, never on `struct` or `enum` declarations (E209). A type parameter is opaque: inside a generic body you may use `==`/`!=` only if `T: Eq`, `<` and friends only if `T: Ord`, and `{x}` or `str(x)` only if `T: Show` (E205).

| Bound | Holds for |
| --- | --- |
| `Eq` | every data type. Floats follow IEEE (`NaN != NaN`). Maps compare as sets of pairs, ignoring insertion order. |
| `Ord` | `bool` (`false < true`), `int`, `float`, `str`, and tuples, lists, `Option`, `Result`, and user structs and enums whose components are all `Ord`. Structs compare field by field in declaration order, enums by variant declaration order and then payload, and `None < Some(_)`. Not `map`, host handles, or host-defined types containing them. |
| `Show` | every data type (used by `{x}`, `print`, and `str(x)`; format in section 9.2) |

- **Float ordering:** the operators `<` `<=` `>` `>=` follow IEEE (any comparison with NaN is false). `sorted`, `sort`, `sorted_by`, `std.math.min`, `max`, and `clamp` use a total order in which NaN is greater than every other float and equal to itself, so sorting is deterministic and never panics.
- **Map keys** must be `int`, `str`, `bool`, opaque host handles, or tuples, structs, or enums built only from those (so `Option[int]` is a valid key). `float` and `list` keys are rejected.
- **Host types** are structs and enums declared in interface files (section 4). A host struct with no body is an opaque handle: it has `Eq` and `Show`, and the plugin can neither construct nor inspect it.

## 3. Declarations

```martite
use geometry.shapes                     // binds the module name `shapes`: shapes.area(…)
use geometry.shapes.Circle              // binds the type `Circle`
use config.MAX_SIZE                     // binds the const `MAX_SIZE`
use app.util as app_util                // alias: binds `app_util` instead of `util`
use other.util as other_util
use app.editor.Doc as EditorDoc         // alias for a single item

pub struct Stack[T] { items: list[T] }

impl[T] Stack[T] {
    pub fn new() -> Stack[T] { Stack { items: [] } }
    pub fn push(mut self, x: T) { self.items.push(x) }
    pub fn peek(self) -> T? { self.items.last() }
}

const MAX_SIZE: int = 1_000

test "push then peek" {
    var s: Stack[int] = Stack.new()     // T cannot be inferred from the call alone (section 6)
    s.push(1)
    assert(s.peek() == Some(1), "peek failed")
}
```

(In a real file `MAX_SIZE` would be declared once; the `use config.MAX_SIZE` line above only illustrates import syntax.)

- **Items are order-independent** at the top level. Locals must be declared before use.
- **Namespaces.** Value-like names (locals, parameters, functions, module names, handler names) are `snake_case`. Consts are `SCREAMING_CASE`. Types and variants are `PascalCase`. Fields and methods live inside their type and never clash with scope names, but a struct may not have a field and a method with the same name (E102).
- **What counts as a value.** Free functions are values and may be passed (`xs.map(double)`). Methods, associated functions (`Stack.new`), and variant constructors (`Some`, `Shape.Circle`) are **not** values; wrap them in a lambda (`fn(x) => Some(x)`).
- **Imports.**
  - `use a.b` binds `b` as a module name. `use a.b.X` binds the single item `X` (a type, a free function, or a const). There is no glob import and no re-export.
  - **Aliases.** `use a.b as c` binds the module as `c` instead of `b`, and `use a.b.X as Y` binds the item as `Y`. The alias must belong to the same casing class as the thing imported (`snake` for modules and functions, `Pascal` for types, `SCREAMING` for consts), otherwise E104. Only the local name changes: the original name is not bound, error messages and `Show` output still use the declared name (`Doc` prints as `<Doc>` even if imported as `EditorDoc`), and type fingerprints ignore aliases. An alias is an ordinary declaration, so the no-shadowing rule applies to it, and an unused alias or import is L001.
  - Variants cannot be imported (`use a.b.Color.Red` is E106, with or without `as`). Variants are written `Color.Red`, except the prelude's, and in patterns the qualifier may be dropped.
  - Types are always referred to by bare name (or their alias), so a type must be imported before it is used in a type position or struct literal. Module-qualified type paths do not exist.
  - Two imports that bind the same name are E102. Aliases are how two modules with the same last path segment (`a.util` and `b.util`) are used together.
- **Visibility:** private by default, `pub` exports. A file is a module. Import cycles are an error. Fields of a `pub` struct and variants of a `pub` enum are public, and there is no per-field visibility. A private type may not appear in a `pub` signature (E106). Methods are private unless marked `pub`.
- **Methods:** `impl` blocks are inherent only (no traits) and must be in the same file as the type they extend (E209). An `impl` has no visibility of its own, and several `impl` blocks per type are allowed. A method's first parameter is `self` or `mut self`. Calls resolve statically on the receiver type, with no overloading.
- **Associated functions** (no `self`) are called as `Stack.new()`. Type arguments are inferred from the arguments or from the expected type, never written at the call site.
- **Struct literals:** `P { x: 1, y: 2 }`, shorthand `P { x, y }`, and update `P { ..p, x: 3 }`. In an update, `..p` must come first, the base is evaluated first, and later fields override. Unit variants are constructed `Color.Red`; payload variants `Shape.Circle(1.0)`. In **patterns** the enum qualifier may be dropped because the scrutinee type is known.
- **Entry points:** a root file is a *script* if it defines `fn main()` or `fn main() -> Result[(), E]` where `E: Show` (an `Err` prints and exits with status 1), and a *plugin* if it defines `on` handlers (section 4). Otherwise it is a library module. A root file may not be both (E407). `on` handlers may appear only in a plugin's root file (E504), and in any other file `main` is an ordinary function.
- **`const`:** the type is required and the initializer must be a constant expression: literals, other consts, operators, tuple/list/map/struct literals, and variant constructors built from constant expressions. No calls, no methods, no `if` or `match`. Consts are evaluated at load in dependency order, and a cycle (E105) or a panic during evaluation (such as `1 / 0` or overflow, E406) is a compile error. Consts may not be used in patterns. There is no mutable global state of any kind; plugin state lives only in `init`'s result.
- **`test` blocks:** `test "name" { … }` has a unique string name (no interpolation) per file, may reach private items of its own file, and is never exported. A test runs as a function returning `()`, so `?` in a test is E207, and a panic fails the test. Tests are checked at load but not instantiated by a host.

## 4. Plugins and the host boundary

A **host** is the application that embeds Martite. It supplies an *interface* (the types, functions, and hooks plugins may use), a *manifest* for each plugin (identity, granted modules, requested limits, settings), and a *limits* configuration. The language knows nothing about any particular application, because everything app-specific lives in those three inputs.

**Interface files.** The host API is written as `.mti` files, parsed by the same parser as source. An embedding can generate them from its Rust registrations or write them by hand. The checker, linter, and editor tooling all read them, so a plugin can be fully checked without the host running. Everything in an interface file is exported, so `pub` is omitted.

```martite
// app/editor.mti, which is module `app.editor`
struct Doc                                    // no body: an opaque handle
struct Selection { start: int, end: int }
enum SaveKind { Manual, Auto }
const MAX_DOC_BYTES: int = 1_000_000          // host constants have literal values

fn text(doc: Doc) -> str                      // no body: a host function
fn selection(doc: Doc) -> Selection
fn replace(doc: Doc, sel: Selection, new_text: str) -> Result[(), str]

on saved(doc: Doc, kind: SaveKind) -> Result[(), str]    // no body: a hook
```

- A body-less `fn` is a **host function**. It is an ordinary typed function (it may be generic), runs synchronously, and may have effects on the host. It can fail only by returning `Result` or `Option`. A fault inside the Rust implementation itself (a bug) surfaces as the `HostFault` trap. Host functions may not declare `mut` parameters (E509); the host boundary is copy-in only.
- A body-less `on` is a **hook**, an event the host may deliver to a plugin. Hook names are unique across the host's interface, and `init` and `shutdown` may not be declared as hooks. Both are checked when the interface loads (E507).
- `struct Name` with no braces is an **opaque handle** (and takes no type parameters). It is an identity token that the host resolves, not a shared mutable object, so copying one copies the token and value semantics still hold inside the plugin. A stale handle makes host functions return `Err` or `None`; it never crashes.
- Interface files have no `impl` blocks. Host APIs are free functions (`editor.text(doc)`), never methods on host types.
- **No function types cross the boundary.** Parameters, return types, and hook payloads cannot mention `fn(…)` (E506), so the boundary carries data only and the host never calls back into a plugin closure. Callbacks are expressed as hooks instead.
- **Values crossing the boundary are copies.** The host and the plugin never share a mutable value.

**Capabilities.** There is no ambient authority. A host module is usable only if the source `use`s it *and* the manifest grants it, and using an ungranted module is E501. A module that no registered interface defines is E106 (this is what a plugin sees for `std.io`, which only `martite run` registers). The `use` lines of host modules, across all files of the plugin, are therefore the plugin's complete reach, readable at a glance by a reviewer or the linter. The pure `std.*` modules need no grant. Grants are per module, so hosts should split their API by risk (for example a read-only module and a separate mutating one). Naming a type from an ungranted module requires a grant, but a plugin may still hold and pass a value of that type whose type it only inferred.

```toml
# plugin.toml
[plugin]
name    = "word-count"         # [a-z0-9-]+, unique within the host
version = "0.2.0"              # informational semver
lang    = "1"                  # language edition
api     = "1.0"                # interface MAJOR.MINOR this plugin needs
entry   = "main.mt"

[permissions]
grant = ["app.editor", "app.status"]

[limits]                       # requests only; the host clamps them
fuel        = 200_000
memory      = 4_194_304        # bytes
depth       = 100
deadline_ms = 20

[settings]                     # optional; delivered to init as Json.Obj
max_words = 5000
```

- **Loading fails (E508)** if the manifest is malformed or has unknown keys, `lang` is unsupported, `api` has a different major version from the host's interface, `api`'s minor is greater than the host's, the entry file is missing, or a granted module does not exist. The entry must be a non-empty relative path inside the project root; absolute paths, drive prefixes, `:`, empty components, and `.`/`..` components are E508. Adding hooks, functions, or types is a compatible change (minor bump). Removing or retyping one is not (major bump).
- **Interface version.** The host interface version is fixed at `1.0` for edition 1 and is not written in interface files. The manifest's `api` major must equal `1`, and its minor must not exceed `0`.
- **Limits** are effective-value = `min(requested, host maximum)`, and an unrequested limit takes the host default.
- **Settings** map TOML to `Json`: tables to `Obj`, arrays to `Arr`, strings, booleans, numbers (all to `Num(float)`). Datetimes are rejected. `Json` must be imported with `use std.json.Json` before it can appear in an `init` signature.

**Handlers.** A plugin is a set of `on` handlers, each implementing one hook.

```martite
use app.editor
use app.editor.Doc
use app.editor.SaveKind
use app.status                          // declares: fn show(msg: str) -> Result[(), str]

struct Stats { saves: int, last_words: int }

on init() -> Stats { Stats { saves: 0, last_words: 0 } }

/// Counts words after each manual save and shows them in the status bar.
on saved(mut self, doc: Doc, kind: SaveKind) -> Result[(), str] {
    if kind == SaveKind.Auto { return Ok(()) }
    let words = editor.text(doc).split(" ").filter(fn(w) => w != "").len()
    self.saves += 1
    self.last_words = words
    status.show("{words} words after {self.saves} saves")
}
```

- `on name(params) [-> R] { … }` implements the hook `name`. The parameters (after an optional `self`) and the return type must match the hook's declaration exactly (E503), though parameter names are free (but still subject to the no-shadowing rule). A handler whose name matches no hook in a granted module is E502. Handler names share the scope namespace with functions, so a `fn` and an `on` with the same name is E102.
- **State is `self`.** `on init() -> S` runs once at instantiation, and its return type `S` (any data type, usually a struct) is the plugin's state type. In every other handler, `self` (or `mut self`) is that state, so `self.saves += 1` updates it. A handler that does not need state may omit `self`. A plugin with no `init` is stateless, and its handlers may not take `self` (E504). The state type must be a data type (E506), which is what lets the host snapshot, persist, and restore it.
- **Settings.** `init` may declare zero parameters, or exactly one parameter of type `Json` (`on init(settings: Json) -> S`), which receives the manifest's `[settings]` table, or `Json.Obj({})` if absent. `init` runs under the normal limits and may call granted host functions. A trap during `init` makes instantiation fail.
- `on shutdown(self)` is the only other lifecycle handler. It returns `()` and runs once at unload. Its state mutations have no effect, and a trap there is reported but the instance is dropped regardless. `init` and `shutdown` are built into the plugin ABI and need no interface declaration, and both names are reserved as hook names.
- Handlers run to completion, one at a time per instance, and are never re-entered. A host function that wants to deliver another event must queue it. A hook the plugin does not implement is simply not delivered, and the host is told so (`Outcome::NoHandler`).
- Handlers are not ordinary functions, so plugin code cannot call them (E505). Inside `test` blocks they are callable by name. The state is passed as the first argument if and only if the handler declares `self`, and as `mut st` if it declares `mut self`:

  ```martite
  test "counts manual saves" {
      var st = init()
      let doc = test_doc()               // handles are supplied by the embedding's test double
      let _ = saved(mut st, doc, SaveKind.Manual)
      assert(st.saves == 1, "saves should be 1")
  }
  ```

  Host functions called from tests are served by the embedding's **test double**, which also supplies any handles. Under plain `martite test` there is no double, so a call to a host function panics with "no test double", and only tests that avoid the host run.
- A handler's return value goes to the host. An `Err` is a normal plugin-level error the host can report, and is different from a trap (section 8).

**One language, many hosts.** `martite run` is itself a host. It runs a file with `fn main` as a script and grants `std.io` and `std.env` to scripts without needing a manifest. Nothing in the core language assumes a plugin.

## 5. Value model and mutability

This is the most important semantic decision. **Everything has value semantics.** Assignment and argument passing behave as copies, so there is no aliasing and nothing to explain about references. In Rust, values are `Rc` with copy-on-write (`Rc::make_mut`), so copies are O(1) until someone mutates. There are no cycles, so reference counting never leaks.

A **place** is a `var` name (or `self` inside a `mut self` method or handler) followed by any chain of `.field`, `.0`, and `[index]` accesses. `x`, `x.f`, `x.items[i].0`, and `self.stats` are places.

| Form | Meaning |
| --- | --- |
| `let x = e` | Immutable binding. Nothing reachable through `x` can change. |
| `var x = e` | Mutable place. `x = …`, `x.f = …`, `x[i] = …`, `x.0 = …`, and `x.push(…)` are all legal. |
| `fn f(xs: T)` | Parameters are immutable bindings. They are always plain names, never patterns. |
| `fn f(mut xs: T)` | Copy-in, copy-out. The call site must write `f(mut p)`, where `p` is a place. |
| `mut self` | Same, but written without `mut` at the call site: `s.push(1)` requires `s` to be a place, otherwise E401. |
| `self` | In a method it is an immutable binding of the receiver, so `self.x = 1` and calling a `mut self` method on it are E401. |
| closures | Capture by value at creation. Assigning to a captured variable is an error (E404). Closures cannot cross the host boundary. |

- **Copy-out** happens on every *normal* exit of the callee: reaching the end of the body, `return`, or an early return by `?`. A panic aborts everything, so there is nothing to copy out.
- **Aliasing rule.** Two arguments of one call (the receiver counts as argument 0) conflict (E403) if either is passed `mut` and both are rooted at the same variable. This is why `f(mut a.x, a.y)` is rejected and `f(mut a, b)` is fine.
- **Assignment.** `p = e` requires `p` to be a place. `x[i] = v` on a `list` panics if `i` is out of range. `m[k] = v` on a `map` **inserts** a new key (appended at the end of iteration order) or overwrites an existing one (keeping its position). `x op= e` (`+= -= *= /= %=`) evaluates the place's operands once. `+=` also works on `str`. Evaluation order is: place operands left to right, then `e`, then the store.
- **Loops iterate a snapshot.** `for p in e` evaluates `e` once and iterates over that value, so mutating the original variable inside the body does not affect the iteration.
- There is no uninitialized variable, since `let x: int` without `=` is illegal. This removes definite-assignment analysis entirely.

**Plugin state.** Between hook calls the host owns the state value. A handler with `mut self` follows the ordinary copy-in, copy-out rule: it works on a copy, and the host stores the result only if the handler returns normally. A panic or any other trap leaves the previous state untouched, so each handler's update to plugin state is atomic with no extra mechanism. An `Err` return is a **normal** return and **does commit** the state, so a handler that wants "no change on error" must validate before it mutates. Calls to host functions are *not* rolled back; see the decisions at the end.

## 6. Static semantics

Checking is **bidirectional**, meaning two modes and no global unification:

```
Γ ⊢ e ⇒ T      infer the type of e
Γ ⊢ e ⇐ T      check e against an expected T
Mode switch:   Γ ⊢ e ⇒ U  and  U ≡ T   gives   Γ ⊢ e ⇐ T      (Never ≡ every T)
```

```
Literals    42⇒int   4.2⇒float   "…"⇒str   true⇒bool   ()⇒()
Interp.     "{e}" requires e ⇒ T with Show(T)
Variable    x:T ∈ Γ                                  ⟹ x ⇒ T
Tuple       eᵢ ⇒ Tᵢ                                  ⟹ (e₁…eₙ) ⇒ (T₁…Tₙ)
List        [] ⇐ list[T];  [e₁, e₂…]: e₁ ⇒ T, rest ⇐ T   ⟹ list[T]   ([None, Some(1)] needs an annotation)
Map         {} ⇐ map[K,V]; first pair infers, rest check
Struct lit  every declared field exactly once, eᵢ ⇐ Fᵢ;  update ..p: p ⇐ S, remaining fields optional
Field       e ⇒ S, f:F ∈ S ⟹ e.f ⇒ F;     e ⇒ (T₀…) ⟹ e.i ⇒ Tᵢ
Index       list[T][int] ⇒ T;   map[K,V][K] ⇒ V;   list[T][range] ⇒ list[T];  str[range] ⇒ str
Range       a, b ⇐ int ⟹ a..b ⇒ range,  a..=b ⇒ range
Arith       + - * /  on (int,int) or (float,float);  % on int only;  str + str ⇒ str
Compare     a ⇒ T, b ⇐ T, Eq(T)  ⟹ a == b ⇒ bool;     Ord(T) for < <= > >=
Logic       and, or, not : bool
Coalesce    a ⇒ Option[T], b ⇐ T ⟹ a ?? b ⇒ T          (b evaluated only if a is None)
Try         e ⇒ Option[T],   enclosing fn/lambda returns Option[_]    ⟹ e? ⇒ T
            e ⇒ Result[T,E], enclosing fn/lambda returns Result[_,E]  ⟹ e? ⇒ T   (same E, no conversion)
If          c ⇐ bool.  Expected T known: both branches ⇐ T.
            Otherwise: a ⇒ T₁, b ⇒ T₂, result is T₁ if T₂ is Never or T₁ ≡ T₂, T₂ if T₁ is Never, else E201.
            No else: a ⇐ () ⟹ ()
Match       s ⇒ S; patterns check against S; arms are joined exactly like if-branches (Never arms are skipped);
            guards ⇐ bool; exhaustive
Lambda      fn(x:A) => e   ⇒ fn(A)->R when e ⇒ R
            ⇐ fn(Ā)->R: parameter annotations optional, body ⇐ R
Call        f ⇒ fn(Ā)->R, args ⇐ Aᵢ ⟹ R      (generic call: see below)
Block       statements in order, Γ grows; value = last statement if an expression, else ()
Return      return e: e ⇐ declared return (the enclosing fn, lambda, or handler);  type Never
Loops       body ⇐ ();  while/for ⇒ ().  A `while true` with no `break` aimed at it ⇒ Never.
```

(The older rule "first branch infers, second checks" would reject `if c { return 1 } else { 2 }`, which is why the `If` and `Match` rows join branches as above.)

**Block values.** A non-final expression statement must have type `()`, and a value you don't want must be discarded explicitly with `let _ = f()`. If the expected block type is `()`, the final expression follows the same rule. This one rule replaces both "unused result" lints and implicit-return surprises.

**Generic calls.** Unification is local to one call expression and type variables never escape it:

1. Instantiate fresh variables for the callee's type parameters.
2. If an expected type is known, unify it with the result type first.
3. Check non-lambda arguments left to right, unifying as you go.
4. Check lambda arguments last, now that their parameter types are known.
5. Check declared bounds against the resolved types (E205).
6. Any variable still unresolved gives E202, "add an annotation".

Methods are calls with the receiver as argument 0, so `xs.map(fn(x) => x + 1)` types without annotations. Because type variables never escape a call, `var s = Stack.new()` is E202 (nothing in that expression fixes `T`) and must be written `var s: Stack[int] = Stack.new()`. The same applies to `let r = Ok(1)` and `let n = None`.

**Patterns:**

- Forms: `_`, literals (`int`, `str`, `bool`, and negative ints; **no float patterns**, no const patterns), bindings, `Variant(p…)`, `(p, q)`, `(p)` for grouping, `S { f, g: p }`.
- A struct pattern must name every field or end with `..` (E204). The `..` rest is last only.
- Or-patterns `A | B` must bind the same names with the same types.
- Guards: `pat if cond =>`. A guarded arm never counts toward exhaustiveness.
- `let`, `for`, and lambda parameter patterns must be irrefutable.
- Matching `int`, `str`, or `float` needs a catch-all arm for exhaustiveness. There are no range patterns, list patterns, or `@` bindings in edition 1.
- Use Maranget's usefulness algorithm for exhaustiveness and redundancy. It powers both E301 and lint L003 from one implementation.

**Scope.** A name may not be declared if any binding with that name is *visible*. Visible means: an enclosing scope, an earlier declaration in the same scope, any top-level item or import of the file (even one written later, since items are order-independent), a prelude name, or a type parameter in scope. Sibling scopes are independent, so two `for i in …` loops in one function are fine. This single rule means every identifier refers to exactly one thing in its function, which makes the linter's job easier.

**Host items.** Interface items are typed exactly like source items, and the checker needs only the `.mti` files, never the running host. Host modules are not in the prelude, so they only occupy names the plugin explicitly `use`s.

## 7. Compile errors

Compile errors cover everything the language makes decidable. The linter only ever sees programs that already pass these.

| ID | Rule |
| --- | --- |
| E001 | Lexical error: bad character, bad escape, unterminated string or interpolation, malformed or out-of-range number |
| E002 | Load limit exceeded (file size, nesting depth, file count) |
| E003 | Syntax error: unexpected or malformed token sequence |
| E101 | Undefined name or type, or `self` used outside a method or handler |
| E102 | Duplicate item, field, variant, parameter, import or alias, test name, or handler/function name clash |
| E103 | Shadowing a visible name |
| E104 | Identifier violates its casing class (including an import alias of the wrong class) |
| E105 | Use before declaration, inside its own initializer, or a const cycle |
| E106 | Unknown module, import cycle, access to a non-`pub` item, import of a variant, or private type in a `pub` signature |
| E201 | Type mismatch (expected and found) |
| E202 | Cannot infer: `[]`, `{}`, `None`, `Ok(…)`, a generic call's type parameter, or a lambda parameter without context |
| E203 | Operator not defined for these types |
| E204 | Wrong argument count, unknown field, missing or duplicate field (literals and patterns) |
| E205 | Bound not satisfied (`Eq`, `Ord`, `Show`, map key) |
| E206 | Non-unit value discarded |
| E207 | `?` in a function whose return type doesn't match |
| E208 | Unknown method |
| E209 | Wrong number of type arguments, bound on a `struct`/`enum` parameter, or `impl` on a type not defined in this file |
| E301 | Non-exhaustive `match` |
| E302 | Refutable pattern in `let`, `for`, or lambda parameter |
| E303 | Or-pattern arms bind different names or types |
| E304 | Pattern doesn't fit scrutinee type, unknown variant, or const/float used as a pattern |
| E401 | Assignment to a `let`, a parameter, an immutable `self`, or a non-place, or a `mut self` call on a non-place |
| E402 | `mut` argument missing, not needed, or not a place |
| E403 | Same variable passed twice where one use is `mut` |
| E404 | Assignment to a captured variable inside a closure |
| E405 | `break` or `continue` outside a loop (a closure body is a boundary) |
| E406 | Constant initializer isn't constant, or panics when evaluated |
| E407 | Missing or malformed `main`, or `main` and `on` in one root file |
| E501 | `use` of a host module the manifest does not grant |
| E502 | `on` handler names no hook in a granted interface |
| E503 | Handler signature differs from its hook |
| E504 | Plugin shape error: duplicate handler, `self` without an `init`, a malformed `init` or `shutdown`, or `on` outside the root file |
| E505 | Handler called outside a `test` block |
| E506 | Function type in the state type or across the host boundary |
| E507 | Interface file error: duplicate hook name, reserved hook name, or an opaque handle with type parameters |
| E508 | Manifest error: malformed, unknown key, unsupported `lang`, `api` mismatch, missing entry, or unknown grant |
| E509 | Host registration error: a host function declared in an interface has no Rust implementation, or the registered signature does not match |

Every diagnostic carries a code, a primary span, a message, and an optional `help` line. Parse errors are E003 and the parser may report several by recovering at item and statement boundaries. Line and column are 1-based; columns count Unicode scalar values, while spans remain byte offsets. Output is human text by default and JSON with `--json` (section 13).

## 8. Dynamic semantics

- **Evaluation** is left to right. `and`, `or`, and `??` short-circuit. Arguments are evaluated left to right, then copy-out happens left to right.
- **Integers:** `/` truncates toward zero, `%` takes the sign of the dividend, and overflow panics. That includes unary `-` and `abs` on the minimum value, `MIN / -1`, and `pow`. `/` and `%` by zero panic.
- **Floats** follow IEEE 754 and never panic: `1.0 / 0.0` is `inf`. Conversions: `float(n)` rounds to the nearest float, and `int(f)` truncates toward zero and **panics** if `f` is NaN or out of range.
- **Maps** iterate in **insertion order**, so output is deterministic. Overwriting a key keeps its position, and removing then re-inserting moves it to the end. `remove` is O(n) so that order is preserved.
- **Slicing** `xs[a..b]` and `s[a..b]` panics unless `0 <= a <= b <= len` (for `..=`, `b` is inclusive).
- **Panics** are uncatchable inside the language. They are index out of bounds, invalid slice, divide by zero, overflow, invalid `int(f)`, `round` with a negative digit count, `replace` with an empty `from`, `expect` on `None`/`Err`, failed `assert`, and explicit `panic(msg)`. A script prints `panic: <msg> at <file>:<line>:<col>` to stderr and exits with status 101 (an `Err` from `main` exits with 1). A plugin hook **traps** (below). There are no exceptions; recoverable errors are `Result` values.
- **`for`** iterates `list[T]` (yields `T`), `map[K,V]` (yields `(K,V)`), `str` (yields 1-char `str`), and `range` (yields `int`).
- **`while true`** is the only infinite loop, and there is no `loop`.

**Traps and limits.** A trap aborts the current hook, discards its `mut self` copy, and returns the trap to the host. The instance stays loaded with its pre-call state, and the host decides policy (log, retry, or disable after repeated traps).

| Trap | Cause |
| --- | --- |
| `Panic(msg, span, trace)` | any panic listed above, with a call trace |
| `OutOfFuel` | the step budget ran out |
| `OutOfMemory` | live value bytes exceeded the cap |
| `DepthExceeded` | call depth, expression nesting, or runtime value nesting exceeded the cap |
| `Deadline` | the wall-clock deadline passed (checked every N fuel ticks and at host calls) |
| `HostFault(msg)` | a host function failed in a way its signature cannot express (a bug in the host) |

- **Limits** are per hook call and set by the host as `Limits { fuel, memory, depth, deadline }`. The manifest may request limits, and the host clamps them. The engine may also enforce a compiled-in depth ceiling so that the interpreter's native stack cannot overflow. The reference interpreter additionally caps runtime value nesting at 128; exceeding it is `DepthExceeded`. That value-nesting ceiling is compiled in and is not part of the host-configurable `Limits`.
- **Fuel** is charged as follows, and the table is fixed within an edition:

  | Event | Cost |
  | --- | --- |
  | each evaluated expression node (including each `while` condition) | 1 |
  | each loop iteration | 1 |
  | any operation that touches `n` list/tuple/map slots (copy-on-write copy, `==`, `Show`, `map`/`filter`/`enumerate`, concatenation) | `n` extra |
  | any operation that touches `b` bytes of a string (concat, slice, compare, `split`, parse, interpolation, `std.json`) | `⌈b/8⌉` extra |
  | `sorted`, `sort`, `sorted_by` on `n` items | `n·⌈log₂(n+1)⌉` extra |
  | `print` | `⌈bytes/8⌉` extra |
  | host function call | 1 plus whatever the host charges with `cx.charge(n)` |

  This is what keeps one built-in call over a huge list from costing one unit of fuel.
- **Memory** counts live bytes of heap payloads: the state, arguments, and locals reachable during the call, including the copy made when copy-on-write actually copies. Shared payloads (several references to one `Rc`) count once. Sizes are nominal and platform-independent: a payload is 16 bytes plus 8 per slot (a map entry is two slots), and a string is 16 plus its byte length.
- **Run to completion.** The language has no threads, timers, sleeping, or blocking, so plugin code can never wait on anything.
- **Determinism.** The core has no clock, randomness, or environment. Given the same host responses, a hook returns the same result (maps iterate in insertion order, and fuel, memory, and `Show` output are all specified). The one exception is `Deadline`, which depends on wall-clock time, so a host that records and replays hook calls should replay with the deadline disabled and rely on fuel. Math functions must use a pure-software libm (not the platform's) so results are bit-identical across machines. Time and randomness are host modules and need a grant, which lets a host record and replay hook calls.

## 9. Standard library surface

This is the normative surface for edition 1. The Rust side declares these as built-in signatures that the checker treats like ordinary generic functions. Where a table says "`mut self`", the method follows section 5.

### 9.1 Prelude

| Item | Meaning |
| --- | --- |
| `print(x: T)` | `T: Show`. Writes the `Show` text plus a newline to the host's log sink (stdout in the CLI, a no-op if the host has none). |
| `panic(msg: str) -> Never` | Aborts with a panic. |
| `assert(cond: bool, msg: str)` | Panics with `msg` if `cond` is false. |
| `int(f: float) -> int`, `float(n: int) -> float`, `str(x: T) -> str` | Conversions. `int` truncates toward zero and panics on NaN or out of range. `str` needs `T: Show`. |
| `Option[T]`, `Result[T, E]` | `enum Option[T] { None, Some(T) }`, `enum Result[T, E] { Ok(T), Err(E) }`. Their variants are usable unqualified. |

### 9.2 The `Show` format (normative)

The text produced by `{x}`, `str(x)`, and `print(x)`:

| Value | Text |
| --- | --- |
| `int` | decimal, `-` for negatives: `42` |
| `float` | the same text as Rust's `{:?}` for `f64`: shortest round-trip form, always with a `.` or an exponent: `2.0`, `0.1`, `1e21`, `NaN`, `inf`, `-inf`, `-0.0` |
| `bool`, unit | `true`, `false`, `()` |
| `str` | the raw text at top level; **inside any container** it is quoted, with `\\ \" \n \t \r` escaped |
| list, tuple, 1-tuple | `[1, 2]`, `(1, "a")`, `(1,)`. Empty list: `[]` |
| map | `{"a": 1, "b": 2}` in insertion order. Empty: `{}` |
| struct | `Point { x: 1, y: 2 }` (declaration order) |
| enum variant | `Color.Red`, `Shape.Circle(1.0)`. Prelude variants are bare: `Some(1)`, `None`, `Ok(())`, `Err("x")` |
| range | `0..5`, `0..=5` |
| opaque handle | `<Doc>` (the type name only, so output stays deterministic) |

### 9.3 Members of built-in types

Methods of generic types are typed like ordinary generic functions with the receiver as argument 0.

| Type | Members |
| --- | --- |
| `list[T]` | `len() -> int`, `get(i) -> T?` (`None` if out of range, including negative), `first`, `last`, `contains(x)` (`Eq`), `map(f)`, `filter(f)`, `fold(init, f)`, `any(f)`, `all(f)`, `enumerate() -> list[(int, T)]`, `reversed()`, `sorted()` (`Ord`, stable), `sorted_by(key: fn(T) -> K)` (`K: Ord`, stable), `join(sep) -> str` on `list[str]`. `mut self`: `push(x)`, `pop() -> T?`, `sort()`, `insert(i, x)` (panics unless `0 <= i <= len`), `remove_at(i) -> T` (panics if out of range), `extend(other: list[T])`, `clear()` |
| `map[K,V]` | `len`, `get(k) -> V?`, `has(k)`, `keys() -> list[K]`, `values() -> list[V]`, `entries() -> list[(K,V)]`. `mut self`: `remove(k) -> V?`, `clear()`. Insert and overwrite with `m[k] = v` |
| `str` | `len`, `chars() -> list[str]`, `split(sep) -> list[str]`, `trim`, `contains(s)`, `find(s) -> int?` (index of first match), `starts_with`, `ends_with`, `replace(from, to)`, `to_upper`, `to_lower`, `parse_int() -> int?`, `parse_float() -> float?` |
| `int` | `abs()`, `pow(e: int)` (panics if `e < 0` or on overflow) |
| `float` | `abs`, `floor`, `ceil`, `trunc`, `sqrt`, `pow(e: float)`, `round(digits: int)` (round half away from zero at that decimal place, `digits >= 0` else panic; digits above 308 leave the value unchanged), `is_nan`, `is_finite` |
| `range` | `len() -> int`, `contains(i: int) -> bool`, `to_list() -> list[int]` |
| `Option[T]` | `is_some`, `is_none`, `map(f)`, `and_then(f)`, `unwrap_or(d)`, `ok_or(e) -> Result[T, E]`, `expect(msg)` |
| `Result[T,E]` | `is_ok`, `is_err`, `map(f)`, `map_err(f)`, `and_then(f)`, `unwrap_or(d)`, `ok() -> T?`, `expect(msg)` |

**Edge cases (normative):**

- `split(sep)` splits on non-overlapping occurrences and keeps empty pieces, so `"".split(",")` is `[""]`, and `split("")` splits into characters. `replace` with an empty `from` panics. `trim` removes Unicode whitespace at both ends.
- `parse_int` accepts `-?[0-9]+` only (no whitespace, no `+`, no underscores) and returns `None` on overflow. `parse_float` accepts `-?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?`, requiring digits on both sides of the decimal point; `.5`, `1.`, and `1.e2` return `None`. Anything else, including `NaN` and `inf`, also returns `None`.
- `unwrap_or(d)` evaluates `d` eagerly. Use `??` for a lazy default.
- `map`, `filter`, `enumerate`, `reversed`, `sorted`, `keys`, `values`, and `entries` are eager and return new lists.
- Indexing `xs[i]` and `m[k]` panics if missing, and `.get` is the safe form. `martite lint` flags unguarded `expect` in plugin code.
- Strings are concatenated with `+`, and building a large string with repeated `+=` is quadratic (it is also fuel-charged), so prefer `list.join`.

### 9.4 Core modules (pure, always available, no grant)

| Module | Contents |
| --- | --- |
| `std.math` | consts `PI`, `EULER`; `min(a, b)`, `max(a, b)`, `clamp(x, lo, hi)` (all `T: Ord`); `sin`, `cos`, `tan`, `atan2(y, x)`, `exp`, `ln`, `log10` on `float` |
| `std.json` | `parse(s: str) -> Result[Json, str]` (nesting deeper than 128 is an `Err`; duplicate object keys: the last value wins, at the first key's position), `stringify(j: Json) -> str` (compact; NaN and infinities become `null`), and `enum Json { Null, Bool(bool), Num(float), Str(str), Arr(list[Json]), Obj(map[str, Json]) }`. `Num` is a `float`, so integers beyond 2⁵³ lose precision. `Json` has `Eq` and `Show`, but not `Ord`. |

`std.json` exists because host payloads and plugin settings are often JSON-shaped. Host modules (section 4) are not part of the standard library, and the checker treats them as separate interface items.

### 9.5 Standalone-host modules (registered by `martite run` only)

| Module | Contents |
| --- | --- |
| `std.io` | `read_line() -> Result[str?, str]` (`Ok(None)` at end of input), `read_file(path: str) -> Result[str, str]`, `write_file(path: str, text: str) -> Result[(), str]` |
| `std.env` | `args() -> list[str]` (script arguments, excluding the script path), `get(name: str) -> str?` |

## 10. Grammar

```ebnf
program    = { item } ;
item       = use_decl | test_decl | on_decl | impl_decl
           | [ "pub" ] ( fn_decl | struct_decl | enum_decl | const_decl ) ;
use_decl   = "use" snake { "." ( snake | Pascal | SCREAMING ) } [ "as" ( snake | Pascal | SCREAMING ) ] ;
test_decl  = "test" STRING block ;                                (* STRING without interpolation *)
on_decl    = "on" snake "(" [ params ] ")" [ "->" type ] block ;  (* self_param allowed *)

fn_decl    = "fn" snake [ tparams ] "(" [ params ] ")" [ "->" type ] block ;
tparams    = "[" tparam { "," tparam } [ "," ] "]" ;
tparam     = Pascal [ ":" BOUND { "+" BOUND } ] ;            (* BOUND = Eq | Ord | Show; bounds only on fn/impl *)
params     = ( self_param | param ) { "," param } [ "," ] ;   (* self_param only first, only in methods/handlers *)
self_param = [ "mut" ] "self" ;
param      = [ "mut" ] snake ":" type ;
struct_decl= "struct" Pascal [ tparams ] "{" [ field { "," field } [ "," ] ] "}" ;
field      = snake ":" type ;
enum_decl  = "enum" Pascal [ tparams ] "{" [ variant { "," variant } [ "," ] ] "}" ;
variant    = Pascal [ "(" type { "," type } ")" ] ;
impl_decl  = "impl" [ tparams ] Pascal [ targs ] "{" { [ "pub" ] fn_decl } "}" ;
const_decl = "const" SCREAMING ":" type "=" expr ;

type       = base [ "?" ] ;
base       = "int" | "float" | "bool" | "str" | "range"
           | ( "list" | "map" | Pascal ) [ targs ]
           | "(" ")" | "(" type "," [ type { "," type } [ "," ] ] ")"
           | "fn" "(" [ type { "," type } ] ")" [ "->" type ] ;
targs      = "[" type { "," type } "]" ;

block      = "{" { stmt } "}" ;                               (* newline-terminated *)
stmt       = ( "let" | "var" ) pattern [ ":" type ] "=" expr
           | "while" expr block
           | "for" pattern "in" expr block
           | "return" [ expr ] | "break" | "continue"
           | expr [ ASSIGN_OP expr ] ;                        (* LHS must be a place: checked after parse *)
ASSIGN_OP  = "=" | "+=" | "-=" | "*=" | "/=" | "%=" ;
CMP_OP     = "==" | "!=" | "<" | "<=" | ">" | ">=" ;

expr       = or_e [ "??" expr ] ;
or_e       = and_e { "or" and_e } ;
and_e      = not_e { "and" not_e } ;
not_e      = "not" not_e | cmp ;
cmp        = range [ CMP_OP range ] ;                         (* non-associative *)
range      = add [ ( ".." | "..=" ) add ] ;                   (* non-associative *)
add        = mul { ( "+" | "-" ) mul } ;
mul        = unary { ( "*" | "/" | "%" ) unary } ;
unary      = "-" unary | postfix ;
postfix    = primary { "(" [ arg { "," arg } [ "," ] ] ")" | "[" expr "]"
                     | "." ( snake | Pascal | SCREAMING | INT ) | "?" } ;
arg        = [ "mut" ] expr ;
primary    = INT | FLOAT | STRING | "true" | "false" | "self"
           | snake | SCREAMING
           | Pascal [ "{" [ struct_init ] "}" ]                (* struct literal *)
           | ( "int" | "float" | "str" ) "(" expr ")"          (* conversions *)
           | "(" ")" | "(" expr ")" | "(" expr "," [ expr { "," expr } [ "," ] ] ")"
           | "[" [ expr { "," expr } [ "," ] ] "]"
           | "{" [ expr ":" expr { "," expr ":" expr } [ "," ] ] "}"   (* map literal *)
           | "if" expr block [ "else" if_tail ]
           | "match" expr "{" { arm } "}"
           | "fn" "(" [ lparam { "," lparam } [ "," ] ] ")" [ "->" type ] ( block | "=>" expr ) ;
if_tail    = block | "if" expr block [ "else" if_tail ] ;
struct_init= ".." expr [ "," field_init { "," field_init } [ "," ] ]
           | field_init { "," field_init } [ "," ] ;
field_init = snake [ ":" expr ] ;
lparam     = pattern [ ":" type ] ;                           (* irrefutable; no `mut`, no `self` *)
arm        = pattern [ "if" expr ] "=>" ( block [ "," ] | expr ( "," | &"}" ) ) ;   (* &"}" = lookahead, not consumed *)

pattern    = pat { "|" pat } ;
pat        = "_" | literal | snake
           | [ Pascal "." ] Pascal [ "(" pattern { "," pattern } [ "," ] ")" ]
           | Pascal "{" [ fpat { "," fpat } [ "," ] ] "}"
           | "(" ")" | "(" pattern ")" | "(" pattern "," [ pattern { "," pattern } [ "," ] ] ")" ;
literal    = [ "-" ] INT | STRING | "true" | "false" ;       (* STRING without interpolation; no floats *)
fpat       = snake [ ":" pattern ] | ".." ;                   (* ".." only last *)

(* interface files (.mti): every item is exported, bodies are omitted, self is not allowed, no impl *)
iface      = { use_decl | iface_item } ;
iface_item = "struct" Pascal                                         (* opaque handle: no "{" on the line *)
           | struct_decl | enum_decl
           | const_decl                                              (* literal initializer only *)
           | "fn" snake [ tparams ] "(" [ params ] ")" [ "->" type ]   (* host function *)
           | "on" snake "(" [ params ] ")" [ "->" type ] ;             (* hook *)
```

**Operator precedence**, loosest to tightest: `??` (right-associative), `or`, `and`, `not`, comparison (non-associative), range (non-associative), `+ -`, `* / %`, unary `-`, then postfix (call, index, field, `?`). So `not a == b` means `not (a == b)`, and `-x.abs()` means `-(x.abs())`.

Notes for the parser:

- A bare `{` in expression position is always a map literal; blocks appear only after keywords. That is why lambdas have two forms: `fn(x) => expr` and `fn(x) { stmts }`. The one exception is a match arm: after `=>` in a `match`, a `{` always starts a block, so a map literal as an arm value must be parenthesized.
- Struct literals are not allowed in the head of `if`, `while`, `for`, or `match` unless parenthesized.
- `a < b < c` and `a..b..c` are parse errors because comparison and range are non-associative.
- A snake identifier in a pattern is always a binding and a Pascal one is always a variant or type.
- Lists in braces require commas, a trailing comma is allowed, and a block-bodied match arm may omit its comma. A missing comma between expression arms is a parse error with a `help` line, since newlines are ignored inside list-style braces.
- A string token contains its interpolations, which are parsed as nested expressions. Nesting counts toward the E002 depth limit.
- `.mti` files are parsed in interface mode, where `fn` and `on` signatures have no body and a brace-less `struct Name` is an opaque handle. A newline ends each signature. Host `fn` parameters may not be `mut`; such a declaration is E509 at load time.
- A lone `-` before an int in a pattern is part of the literal. In expressions it is unary minus.

## 11. Embedding API

This is an illustrative sketch of the Rust surface, and the exact signatures are not part of the language design.

```rust
let mut engine = Engine::new(HostLimits { max: Limits { fuel: 1_000_000, memory: 8 << 20, depth: 200,
                                                        deadline: Duration::from_millis(50) },
                                          default: Limits { fuel: 100_000, memory: 1 << 20, depth: 100,
                                                            deadline: Duration::from_millis(10) } });

engine.interface(include_str!("app/editor.mti"))?;
engine.host_fn("app.editor.text", |cx, args| { /* ... */ })?;

let plugin = engine.load(manifest, &sources)?; // sources: impl SourceProvider; parse + check; every E-code surfaces here
let mut inst = plugin.instantiate()?;          // runs `init(settings)`, owns the state
match inst.call("saved", args) {               // runs one hook
    Ok(Outcome::Handled(value)) => { /* use the hook's return value */ }
    Ok(Outcome::NoHandler)      => { /* the plugin does not implement this hook */ }
    Err(Trap::OutOfFuel)        => { /* host policy */ }
    Err(trap)                   => { /* log it, state is unchanged */ }
}
let snap = inst.snapshot();                    // state is a data type, so it serializes
```

- **Load once, instantiate many.** `load` produces an immutable checked program that instances share. An instance is just the state plus its limits.
- **Interface and implementation are tied together.** Every host function declared in an `.mti` must have a Rust implementation with a matching type at load time, and a missing or mistyped one is a load error (E509), not a runtime surprise.
- **Threads.** Values are `Rc`, so an instance is `!Send`. A host that runs plugins on several threads uses one instance per thread, or enables `Arc`-backed values behind a cargo feature.
- **Host functions charge fuel.** Host code can call `cx.charge(n)` to account for expensive work, since the interpreter cannot see inside it. A Rust-side failure returns `HostFault`.
- **Snapshots** are a versioned, bounded binary encoding of the state value together with a **type fingerprint**: a BLAKE3 hash over the names and shapes of every type reachable from the state type (field names and types, variant names and order, type parameters), ignoring doc comments and `pub`. The reference encoding stores numeric type IDs rather than a self-describing schema and validates every decoded type, variant, field count, and value against the checked program; a snapshot whose format version or fingerprint does not match is rejected. Opaque handles are encoded as host-defined tokens; the host is responsible for treating those tokens as valid only for one host session (the reference format does not add a separate session tag).
- **Reload.** The host re-checks the new source and compares fingerprints. If they are equal it restores the snapshot without running `init`; otherwise it re-runs `init` (the host may instead choose to refuse the reload).
- **Test doubles.** The embedding can ship a test harness that registers fake host functions and supplies handles, so `test` blocks in plugins run offline.

## 12. Linter design

The linter is a set of rules over one immutable analysis result. Rule IDs and meanings are fixed for the edition. Severities, thresholds, groups, and presets are defaults that tools may tune.

### 12.1 Terms, analyses, and pipeline

**Terms.** These definitions are normative for the rules in 12.5 and 12.6.

- **Place.** A local, a parameter, or `self`, followed by any chain of `.field`, `.0`, and `[index]` projections. A `mut` parameter is a place; an immutable binding and `self` in a non-`mut self` method are places that cannot be written.
- **Side-effect-free place.** A place whose index expressions are themselves side-effect-free expressions. Evaluating it does not call a function or mutate anything, though an index can still panic.
- **Side-effect-free expression.** A literal, a side-effect-free place, a tuple/list/map literal whose elements are side-effect-free, a unary minus applied to a numeric literal, or a field/tuple projection of one. Calls, conversions other than `float(int)`, interpolation, `if`, `match`, blocks, and lambdas are not side-effect-free.
- **Stable expression.** An expression whose repeated evaluation yields the same value with no side effects and no panic: a side-effect-free expression, or a chain of builtin methods that are not `mut self`, take no function argument, and are not listed in section 9.3 as panicking (`len`, `get`, `first`, `last`, `is_some`, `is_ok`, `find`, `contains`, and the like). `expect`, integer `abs`, integer `pow`, `round`, `replace`, indexing, and slicing are not stable. L008 uses this definition so that a guard such as `words.first().is_some()` can match a later `words.first().expect(…)`.
- **Mutation.** A write to a place rooted at a binding: `=`, `op=`, a field/index/tuple store, passing the place as a `mut` argument, or calling a `mut self` method on the binding or a projection of it. Copy-out through a helper counts. Closures capture by value, so a write inside a closure is not a mutation of the enclosing binding (and is E404).
- **Reachability.** The roots of a program are its `pub` items in every loaded module, its `on` handlers, `main`, and its `test` blocks. A function, method, or lambda is reachable when it is a root, is called, is used as a value, or is passed to a call whose callee may invoke it, transitively. Methods resolve statically and host functions are leaves. A lambda body is reachable when its lambda expression is evaluated in reachable code. This call graph over-approximates on purpose: any function used as a value is treated as callable, and any lambda is treated as callable once created.
- **Panic-capable operation.** An operation the specification says can panic: index out of bounds, invalid slice, division or remainder by zero, integer overflow (including unary `-`, `abs`, `MIN / -1`, and integer `pow`), invalid `int(f)`, `round` with a negative digit count, `replace` with an empty `from`, `expect` on `None`/`Err`, a failed `assert`, and explicit `panic`.
- **Loop exit.** For a `while` loop, a `break` whose nearest enclosing loop is that loop, or a `return` or `?` belonging to the function that encloses the loop. A `break` in a nested loop does not exit the outer loop, and a `return`, `?`, or `break` inside a lambda or nested function does not exit it. A `panic` is not an exit. Exits are judged on the control-flow graph, ignoring condition values.
- **Guarded `expect` (L008).** An `.expect(…)` is guarded when it sits in the then-branch of an `if` whose condition tests `e.is_some()` or `e.is_ok()`, or in the right operand of an `and` whose left operand is such a test, where `e` is a stable expression and no mutation of a root of `e` occurs between the test and the call.
- **Doc-able element.** A top-level item, an `impl` method, a struct field, or an enum variant.

**Analyses.** The linter builds one immutable analysis result containing, in addition to spans, comment trivia, and parenthesis flags: binding, definition, and import tables and mutation facts (resolved); expression, pattern, state-type, and host-type tables (typed); per-arm usefulness results (patterns); the call graph and reachability, loop exits, and possible `Err` return paths (flow); parsed `plugin.toml` and `.mti` inputs and grants (manifest); and the emitted diagnostics (post).

**Contracts the Rust core should guarantee:**

- Every AST node has a `NodeId` and a `Span`; tokens carry comment trivia; and every expression records whether it was parenthesized in source.
- Resolver and checker output live in side tables keyed by `NodeId`: binding for each identifier, type for each expression, use and assignment counts per binding, and the call, mutation, and return facts needed by the flow tier.
- A rule is `fn check(&Ctx, &mut Emit)`, and rules never mutate the tree. A fix is a list of `Edit { span, replacement }`, which `--fix` can apply safely because spans are exact.
- Rules are tiered by the analysis they need, so the first rules can be written before the checker exists.
- Output is deterministic and sorted by file path, then span, then rule ID, then message.
- The linter also reads `.mti` files and `plugin.toml` as inputs, so host-aware rules work offline, without the host. It produces diagnostics only for `.mt` files and, for L020, `plugin.toml`; `.mti` files are never lint targets.

**Pipeline.** The linter runs only on programs that pass every E-check, so it never duplicates a compile error. In a full run, if any E diagnostic exists in a program, `martite lint` prints those and runs no rules for that program. `--syntax-only` is the exception, meant for editors: it runs only the syntax tier on any file that parses, even while the rest of the project is broken, and it loads no interfaces or manifest. With several paths, each program is loaded separately, files reached by more than one entry are linted once, and a broken program does not block linting the others.

**Tiers.** A rule declares exactly one tier: the richest analysis it uses. Every guarantee of that tier and of its listed dependencies is available; a rule never requires an analysis outside it.

| Tier | Guarantees |
| --- | --- |
| syntax | tokens, AST, spans, comments/trivia, parentheses, literal values |
| resolved | + bindings, definitions, imports, method resolution, local mutation facts |
| typed | + expression, pattern, and type tables; state type; entry kind; host types |
| patterns | + per-arm usefulness for `match` (also typed) |
| flow | + call graph and reachability, loop exits, possible `Err` paths (also typed) |
| manifest | + parsed `plugin.toml` and `.mti` inputs, grants (also resolved) |
| post | + all diagnostics emitted by enabled non-post rules (full runs only) |

The `Tier` column in 12.5 and 12.6 assigns every rule. Under `--syntax-only`, only rules in the syntax tier run.

**Stability.** Rule IDs are append-only and never reused. A retired rule leaves a tombstone, and changing what a rule means requires a new ID. Adding a rule is not an edition change. Rules that apply only to plugins (L008, L019, L020, L021, L026, L027, L031) are skipped for scripts and libraries.

### 12.2 Severity, groups, and config

Severities are `error`, `warn`, `info`, and `off`. `martite lint` exits 1 if any `error` diagnostic is reported, or any `warn` when `--deny-warnings` is given. `info` is shown but never affects the exit code.

Resolution is a total order:

1. The preset gives every rule a base severity.
2. `[lint.groups]` overrides each rule in the named group.
3. `[lint.severity]` overrides each named rule.
4. Matching `[[lint.override]]` entries apply in array order; within one entry its `preset`, then `groups`, then `severity`, then `off`.
5. `--rule` restricts which rules run and does not change severities.

An override may set `preset`, `groups`, `severity`, `thresholds`, `options`, and `off`; an override's `preset` replaces the base preset for matching paths before its other fields apply, and `off` is shorthand for severity `off`. Listing the same rule in both `off` and `severity` in one override is a usage error. An override's `paths` are globs relative to the directory containing `martite.toml`: `*` matches within one path segment, `**` matches any number of segments, and `?` matches one scalar value. Matching is case-sensitive; later matching entries win.

Groups: `correct` (probably a bug), `simplify` (equivalent, simpler code), `style`, `plugin` (host and sandbox concerns), `perf`, `docs`.

Presets:

- `minimal`: every rule `off`, then every rule in `correct` at `warn`, then L010 and L019 at `error`.
- `recommended`: the default column in 12.5 and 12.6. This is the default preset.
- `strict`: every rule enabled; rules whose default is `warn`, `info`, or `off` become `warn`, while rules whose default is `error` stay `error`.

```toml
# martite.toml
[lint]
preset = "recommended"

[lint.groups]
simplify = "info"

[lint.severity]
L018 = "warn"
L026 = "off"

[lint.thresholds]
max_fn_lines = 60          # L017
max_params   = 5           # L017, not counting self
max_nesting  = 4           # L017
max_branches = 12          # L017

[lint.options]
float_eq_zero_ok = true    # L007

[[lint.override]]          # per-path adjustments
paths = ["tests/**"]
off   = ["L008", "L018"]
```

Unknown keys, unknown rule or group IDs, unknown severity or preset names, and bad values in `martite.toml` are a usage error (exit 2). Thresholds are `max_fn_lines >= 1`, `max_params >= 0`, `max_nesting >= 0`, and `max_branches >= 1`. Tombstoned rule IDs are accepted and ignored, and `--list-rules` marks them as retired; any other unknown rule ID is a usage error.

### 12.3 Suppression

`// allow(L001, L002)` suppresses those IDs for diagnostics whose primary span starts within the head of the next syntactic element (section 1). The element is a statement, a struct field, an enum variant, an `impl` method, a match arm, or an item. Its head is the whole element for simple statements, fields, variants, `const`, and `use`, and everything through the opening `{` for other items, `impl` methods, `if`, `while`, `for`, `match`, and block lambdas. So a suppression above an `fn` covers its signature but not its body, a suppression above an `impl` method covers its signature but not its body, and a suppression above a multi-line call covers every line of the call. Scoping to the element rather than the next line is what keeps suppressions valid when `martite fmt` reflows code.

Blank lines between a suppression and its element do not detach it. Several `// allow` comments directly above one element combine. Only diagnostics in `.mt` files are suppressible; L020 in `plugin.toml` and any `.mti` diagnostic are not. E-codes cannot be suppressed, and neither L022 nor L032 is suppressed by another comment.

A suppression that names an unknown ID, names an E-code, or suppresses nothing is reported by L022. L022 considers only the rules enabled for the run: a suppression naming a known rule that is `off` or excluded by `--rule` does not suppress nothing. L022 itself runs only when L022 is enabled.

#### 12.3.1 Primary spans

Each diagnostic's primary span is the anchor below. When a rule has several possible spans, this is the one used for suppression.

| Rule | Primary span |
| --- | --- |
| L001 | The unused binding or parameter name; for an import or alias, its local name; for a private item, its declared name. |
| L002 | The `var` keyword. |
| L003 | The redundant arm's pattern. |
| L004 | The first unreachable statement. |
| L005 | The condition expression; for a constant operator, the operator that makes it constant. |
| L006 | The operator. |
| L007 | The comparison operator. |
| L008 | The `.expect` callee. |
| L009 | The `let` pattern. |
| L010 | The zero divisor. |
| L011 | The expression being simplified. |
| L012 | The assignment operator. |
| L013 | The `return` keyword; for the else form, the `if` keyword. |
| L014 | The match scrutinee. |
| L015 | The `mut` keyword. |
| L016 | The block's opening brace. |
| L017 | The item's name; for a lambda, the `fn` keyword. |
| L018 | The item's declared name. |
| L019 | The `while` keyword. |
| L020 | The unused grant string in `plugin.toml`. |
| L021 | The offending type occurrence in user code; if the handle is reachable only through a host type, the user-code type site naming that host type. |
| L022 | The suppression comment. |
| L023 | The `unwrap_or` call. |
| L024 | The outermost mixed operator. |
| L025 | The assignment operator. |
| L026 | The offending operation. |
| L027 | The earliest mutation of state that can reach an `Err` exit. |
| L028 | The identifier read. |
| L029 | The lambda expression. |
| L030 | The test name. |
| L031 | The `print` callee. |
| L032 | The `///` comment. |

### 12.4 Fixes

A fix is a list of edits with an `applicability`: `machine` (always behavior-preserving, applied by `--fix`) or `maybe` (needs a human check, applied only by `--fix --suggested`). `--fix --suggested` applies both. Suppressed diagnostics produce no fixes.

- Fixes are applied project-atomically: collect fixes for enabled, unsuppressed diagnostics, apply them to an in-memory overlay for the whole program, re-parse and re-check, and write files only after a pass that introduces no E-code.
- Overlapping edits are resolved by keeping the one with the earlier start (ties go to the lower rule ID), and the linter re-runs, up to 5 passes. An insertion at the same offset as another edit conflicts with it and is resolved by the lower rule ID.
- If a pass introduces any E-code, the linter discards that pass's fixes for every file, reports the introduced E diagnostics, and stops. A partially applied project is never written.
- L015 follows the same rule: its declaration edit and all call-site edits in other files succeed or fail together.
- Deleting a `use` statement removes its line and any trailing same-line comment, but not leading own-line comments. Those become L022's responsibility on a later pass.
- `--fix` does not format. Run `martite fmt` afterwards. No lint rule talks about whitespace, so the two tools never conflict.
- `--fix` and `--syntax-only` are mutually exclusive (usage error). `--fix --json` reports the diagnostics that remain after fixing. The exit code is computed from those remaining diagnostics.

### 12.5 Rules

Defaults are for the `recommended` preset. "Fix" is M (machine), S (maybe), or none.

| ID | Fires when | Notes and exemptions | Tier | Fix | Group / default |
| --- | --- | --- | --- | --- | --- |
| L001 | A binding, parameter, import, alias, or private item is never used. A binding that is only ever assigned and never read counts as unused. | `_` prefix opts out. A private item is unused when unreachable from every root. Roots are `pub` items in every loaded module, `on` handlers, `main`, and `test` blocks, so mutually recursive dead code is found. Uses inside `test` blocks count. `self` is never flagged. | flow | M for imports (delete line), S for bindings (add `_`) | style / warn |
| L002 | A `var` is never mutated: never written, never passed `mut`, and never receives a `mut self` call on itself or a projection. | `mutation` is defined in 12.1. | resolved | M (`var` to `let`) | simplify / warn |
| L003 | A match arm is redundant or unreachable (Maranget usefulness), including a guarded arm already covered by an earlier unguarded one and any arm after a catch-all. | A guarded arm that is not covered is never flagged. | patterns | S (delete arm) | correct / warn |
| L004 | A statement follows `return`, `break`, `continue`, `panic(…)`, or a `while true` with no `break`. | Prelude names cannot be shadowed, so matching `panic` by name is sound. A statement-level expression of type Never counts too. | typed | S (delete) | correct / warn |
| L005 | A condition is a literal `true` or `false`, or `not`, `and`, or `or` applied to literals, in `if`, `while`, or a boolean operator. | `while true` is exempt. A SCREAMING const (`if DEBUG`) is exempt, since it is a deliberate switch. | syntax | S (keep the live branch) | correct / warn |
| L006 | A self-comparison or self-assignment on a side-effect-free place or expression: `x == x`, `x != x`, `x = x`, `a.f = a.f`. | Calls are never side-effect-free for this rule. Help for `x != x` on floats suggests `is_nan()`. | resolved | none | correct / warn |
| L007 | `==` or `!=` where an operand is a `float`. | Comparing against the literal `0.0` is allowed while `float_eq_zero_ok = true`. Floats nested in tuples or structs are not inspected. | typed | none | correct / warn |
| L008 | `.expect(…)` outside `test` blocks that is not guarded. | Plugin code only. Guarded is defined in 12.1; it uses stable expressions, so `words.first().is_some()` guards a later `words.first().expect(…)`. In plugin code the help says a trap aborts the current hook and suggests returning `Err`. | typed | none | plugin / warn |
| L009 | `let _ = e` where `e` is a `Result`. | Exempt in `test` blocks. Help suggests `?`, an explicit `match`, or `if r.is_err()`. Note that `.ok()` still discards the error. | typed | none | correct / warn |
| L010 | Integer `/` or `%` by a literal zero (`0`, `0x0`, `0b0`, `-0`). | Float division by `0.0` is legal and is not flagged. In a full run the well-typedness of the program makes the integer case unambiguous. | typed | none | correct / error |
| L011 | `x == true`, `x != false`, `x == false`, `x != true`, `if c { true } else { false }` and its inverse, `not (a == b)`, `not (a != b)`. | `not (a < b)` is deliberately not simplified, because it is not equivalent for NaN. | syntax | M | simplify / info |
| L012 | `x = x + n` (also `-`, `*`, `/`, `%`) where `x` is a side-effect-free place. | `+=` evaluates place operands once, so `a[f()] = a[f()] + 1` is not flagged. `x = n + x` is not flagged. Not flagged when `n` contains a `mut` argument or `mut self` call rooted at `x`. | resolved | M | simplify / info |
| L013 | `return e` or bare `return` as the last statement of a fn, lambda, or handler body. An `else` branch that follows an `if` whose branch ends in `return`, when the `if` is a statement. | A `return` inside a loop or `match` arm is never flagged. | syntax | M for final `return`, S for `else` | simplify / info |
| L014 | A `match` on a `bool` with arms `true` and `false`, or `true` and `_`. | | typed | S | simplify / info |
| L015 | A `mut` parameter or `mut self` that is never mutated in the body. | `mutation` is defined in 12.1. A fix is offered only for private functions and methods, where every call site is in the project: it edits the declaration and all call sites. Handlers and `pub` items get no fix. | resolved | S | correct / warn |
| L016 | An empty block `{}`. | Exempt if it contains a comment. Struct, enum, and map braces are not blocks. A `=> ()` match arm is the preferred way to say "nothing". | syntax | none | style / info |
| L017 | A function, method, handler, or lambda exceeds a threshold: lines (blank and comment-only lines excluded), parameters (excluding `self`), nesting depth of blocks, `if`, loops, and `match`, or branch count (1 + each `if`, loop, extra match arm, `and`, `or`, `??`). | One diagnostic per exceeded metric. Lambdas count toward the enclosing item's totals and add one nesting level. `test` blocks are exempt from the line limit. | syntax | none | style / info |
| L018 | A `pub` fn, method, struct, enum, or const has no `///` doc. | Variants, fields, `main`, and `.mti` items are exempt from the requirement, but variants and fields are doc-able (12.1). | syntax | none | docs / off |
| L019 | A `while true` in a handler, or in a function reachable from one, with no exit. | An exit is defined in 12.1. Such a loop can only end in `OutOfFuel`. A `panic` does not count. | flow | none | plugin / error |
| L020 | The manifest grants a module that no file of the plugin `use`s (least privilege). | A grant for module `m` is used by `use m` and by `use m.X` for an item declared in `m`; a submodule import does not use the parent's grant. Uses inside `test` blocks count. Span points into `plugin.toml`. Fix removes the grant entry. | manifest | S | plugin / warn |
| L021 | An opaque host handle appears anywhere in the state type, directly or through a container, tuple, generic, or user or host struct or enum. | A plugin with no `init` has no state type, so the rule does not fire. The span is the offending type occurrence in user code; if the handle is reachable only through a host type, the span is the user-code site naming that host type. Help suggests storing an id and re-resolving it. The handle is stale after a restart or restore. | typed | none | plugin / warn |

### 12.6 Additional rules (L022 to L032)

| ID | Fires when | Notes and exemptions | Tier | Fix | Group / default |
| --- | --- | --- | --- | --- | --- |
| L022 | A suppression names an unknown ID, names an E-code, or suppresses nothing. | Runs last in full runs and never under `--syntax-only`. Considers only enabled rules; a known rule that is `off` or excluded by `--rule` does not suppress nothing. A retired ID counts as unknown in a suppression. L022 is not suppressible. | post | M (remove ID or comment) | correct / warn |
| L023 | `Option.unwrap_or(d)` where `d` is a call, a string or collection literal, interpolation, or anything that allocates or can trap. | `unwrap_or` evaluates `d` eagerly, while `??` is lazy. Scalar literals, plain names, and field/tuple projections are exempt. Everything else is treated as possibly allocating or trapping. The fix changes when `d` is evaluated, so it is `maybe`. `Result.unwrap_or` is exempt because `??` is `Option`-only. | typed | S (to `??`) | perf / info |
| L024 | `and` and `or` mixed in one expression without parentheses (`a and b or c`). | Parentheses are recorded in the syntax tier. Precedence is well defined, but the mix is easy to misread. The formatter never adds parentheses, so this fix does. | syntax | M | style / info |
| L025 | `s += …`, `s = s + …`, or `s = … + s` on a `str` inside a loop body. | Repeated concatenation is quadratic and fuel-charged. Help suggests collecting into a `list[str]` and calling `join`. | typed | none | perf / info |
| L026 | A panic-capable operation in a handler or in a function reachable from one: `xs[i]` and `m[k]`, slicing, `/` or `%` by a non-constant, integer overflow (`+`, `-`, `*`, unary `-`, `abs`, `MIN / -1`), `int(f)`, integer `pow`, `insert`, `remove_at`, `round` with a possibly negative digit count, `replace` with a possibly empty `from`, `panic`. | Opt-in. A trap here aborts the whole hook. `assert` is exempt as a deliberate check; `expect` is covered by L008. The list follows section 8 and the panic notes in section 9.3. | flow | none | plugin / off |
| L027 | A `mut self` handler (or a helper taking the state `mut`) mutates `self` and later returns `Err`, via `return Err(…)` or `?`, on some path. | Analyzed on the control-flow graph, with summaries for helpers that take the state `mut`; condition values are ignored. `Err` is a normal return and commits state (section 5). Help says to validate before mutating. Only for `Result` return types. | flow | none | plugin / warn |
| L028 | A `_`-prefixed binding is read. | The prefix says "unused", so a read is misleading. Fix renames the binding and its uses. | resolved | S | style / info |
| L029 | `fn(x) => f(x)` where `f` is a free function and the arguments pass through unchanged. | Methods and variant constructors are not values, so wrapping them is required and exempt. | resolved | S (replace with `f`) | simplify / info |
| L030 | A `test` block contains no `assert` call and calls no function that does. | Asserts are counted lexically in the test and transitively in free functions it calls. Handler calls count only when the test asserts afterward. | flow | none | correct / warn |
| L031 | `print` in plugin code outside `test` blocks. | The host's log sink may be a no-op, so this is usually a leftover debug call. Needs the entry kind, so it does not run under `--syntax-only`. | typed | none | plugin / info |
| L032 | A `///` comment that does not directly precede a doc-able element. | Doc-able elements are defined in 12.1. Section 1 makes other `///` comments ordinary. L032 is not suppressible. Fix rewrites it as `//`. | syntax | M | docs / warn |

### 12.7 Examples for the subtle rules

L027: the state is committed even though the handler fails.

```martite
on added(mut self, item: str) -> Result[(), str] {
    self.count += 1                          // L027: this mutation is committed
    if item == "" { return Err("empty") }    // Err commits the new count
    Ok(())
}
```

The fix is to validate first and mutate last:

```martite
on added(mut self, item: str) -> Result[(), str] {
    if item == "" { return Err("empty") }
    self.count += 1
    Ok(())
}
```

L008 and a guard (not flagged):

```martite
if words.first().is_some() { let w = words.first().expect("checked") }
```

L019 (the only exit is `OutOfFuel`):

```martite
on tick(mut self) {
    while true { self.n += 1 }               // L019, error by default
}
```

### 12.8 Output and CLI

Human output shows the severity and code in the header, a source excerpt with the primary span underlined, then `help`:

```
warn[L002]: variable `total` is never mutated
  --> main.mt:12:5
   |
12 |     var total = 0
   |     ^^^ use `let`
   = help: replace `var` with `let` (machine fix)
```

JSON diagnostics keep the section 13 shape and gain `group`. `fix`, when present, is a list of edits; each edit carries `applicability` (`machine` or `maybe`), and every edit of one diagnostic has the same applicability. A lint diagnostic in `plugin.toml` (L020) uses that file in `file`, with byte offsets into it. No lint rule reports a diagnostic in an `.mti` file.

`martite lint [paths]` lints `.mt` files. With no paths it uses the project rooted at the nearest ancestor of the working directory containing `martite.toml` or `plugin.toml`, and the manifest entry when present; if there is no manifest entry, every `.mt` file under that root is linted as its own entry. If neither file exists, the working directory is the root and a path is required. With paths, each path is loaded as `martite check` loads it; the loaded files are linted once per project root, and duplicate files are linted once. `.mti` files are read as inputs through the interfaces supplied by the embedding or `--interface`; they are not lint targets.

Flags: `martite lint [paths]`, `--fix`, `--fix --suggested`, `--json`, `--deny-warnings`, `--syntax-only`, `--rule L001,L002`, `--interface <file>` (repeatable), `--list-rules`, and `--explain L019`.

- `--syntax-only` runs only syntax-tier rules on the named files; it loads no interfaces, manifest, or other modules.
- `--rule L001,L002` selects the exact set of rules to run; L022 runs only when it is selected, and its suppressed-nothing check considers only the selected rules. An unknown ID or a retired ID in `--rule` is a usage error.
- `--list-rules` prints one line per ID with its group, default severity, and whether it is retired; with `--json` it prints an array of `{id, group, default_severity, retired}`.
- `--explain L019` prints that rule's rationale and examples from the built-in registry; it does not load a project.
- `--fix` and `--syntax-only` are mutually exclusive. `--fix` does not format.

Diagnostics are sorted by the displayed file path (lexicographic by Unicode scalar value), then start byte, then rule ID, then message. Exit codes are 0, 1, and 2 as in section 13; `--syntax-only` reports parse errors as E-codes and exits 1 if any are present.

Presets, groups, severities, thresholds, and options live in `martite.toml`. Because the compiler already rejects shadowing, casing errors, type errors, and non-exhaustive matches, the lint set is almost entirely about simplification and suspicious code.

## 13. Tooling and conformance

- **Commands:** `martite run <file>` (script), `martite check <path>` (parse and type-check; with a plugin directory it also reads `plugin.toml` and the interfaces given by `--interface`), `martite lint` (flags in section 12.8), `martite test`, and `martite fmt` (flags in section 14.8). Tool settings (formatter options, lint severities, groups, thresholds, options, and per-path overrides) live in `martite.toml`.
- **Test discovery:** `martite test <path>` loads the entry module and every module reachable through `use`, then runs every `test` block in that program. There is no name filter. It supplies no host test double, so a test that calls a host function traps. The command exits 1 if any test fails.
- **Exit codes:** 0 success, 1 diagnostics found (an `error`, or a `warn` under `--deny-warnings`), a test failed, `martite fmt --check` found a difference, or `main` returned `Err`, 2 usage or load error, 101 panic or any other trap in a script.
- **JSON diagnostics** (`--json`) are one object per diagnostic: `code`, `severity`, `message`, `file`, `span` (`start` and `end` byte offsets and 1-based `line`/`column`, where a column counts Unicode scalar values), optional `help`, and optional `fix` (a list of `{span, replacement, applicability}` edits). Lint diagnostics also carry `group`.
- **Conformance suite.** The reference implementation is considered complete only when it ships, and passes: at least one failing and one passing fixture for every E-code and every lint rule; one fixture at each rule's declared tier and a `--syntax-only` fixture showing higher-tier rules do not run; golden fixtures for preset, group, and override precedence; golden tests for the `Show` format (section 9.2); golden tests for the fuel and memory tables (section 8); grammar fixtures for every parser note in section 10; and a fuzz run over the lexer, parser, and checker confirming that no input panics the host or exceeds the load limits.
- **Formatter and linter conformance.** The suite also includes: golden before/after fixtures for every row of sections 14.3 to 14.6; an idempotence check (`fmt(fmt(x)) == fmt(x)`) and an AST-equality check over the whole fixture corpus; a test that comments survive from every position the grammar allows; one fixture per documented exemption in sections 12.5 and 12.6; one fixture per primary-span and suppression case in section 12.3, including L022 under `--rule`; a project-atomic multi-file fix test for L015 and a fix-discard test where a fix would introduce an E-code; for every rule with a fix, a test that applying it removes the diagnostic, introduces no E-code, and leaves results unchanged under a deterministic test run; a golden JSON test covering `group` and per-edit `applicability`; a test that `--fix` followed by `fmt` is stable; and a test that formatting never changes which diagnostics are suppressed.

## 14. Formatter (`martite fmt`)

### 14.1 Guarantees

- **One canonical style.** Output depends only on the token stream, the comments, and three config values (`max_width`, `indent`, `end_of_line`). There are no per-construct knobs.
- **Idempotent.** `fmt(fmt(x)) == fmt(x)`.
- **Meaning-preserving.** The formatted file parses to the same AST as the input, apart from the explicit normalizations below (trailing commas, import order, numeric literal case, comment spacing). The formatter never adds or removes parentheses, never rewrites string contents or interpolations, and never converts between `fn(x) => e` and `fn(x) { e }`. `martite fmt --verify` re-parses the output and compares ASTs, and a mismatch is a formatter bug that refuses to write the file.
- **Total on valid input.** Any file that parses (no E001 to E003) can be formatted, even if it has type errors. A file with a syntax error is reported and left untouched.
- **Comments are never lost, merged into a line, or moved across a statement boundary.**

### 14.2 Where a line may break

Newlines are significant (section 1), so the formatter may only break where a newline is ignored:

- after a binary operator (including `|` in or-patterns), an assignment operator, `,`, `:`, `=>`, `->`, or an opening bracket;
- before `.`, `else`, `and`, `or`, or `??`;
- anywhere inside `()`, `[]`, or a list-style brace.

It never breaks between `return` and its value, before `?`, after unary `-` or `not`, or inside `..` and `..=`.

### 14.3 Layout rules

Width is measured in Unicode scalar values, with a default `max_width` of 100. Indentation is spaces only, 4 per level by default. The printer is a standard group-based fit-or-break pretty printer: a group is printed flat if it fits in the remaining width, otherwise it breaks, and breaking an outer group never breaks an inner one that still fits. A token that cannot break (a long string, say) may exceed the width.

| Construct | Rule |
| --- | --- |
| Comma lists (arguments, parameters, tuples, list and map literals, struct literal fields, type parameters and arguments, struct and enum bodies) | Flat if it fits. Otherwise one element per line, indented once, trailing comma added, closing bracket on its own line. Flat lists never have a trailing comma, except the 1-tuple `(e,)` and `(T,)`, which always keep theirs. |
| Spacing | One space around binary operators, `=`, `=>`, `->`, and after `,` and `:`. No space before `:` or `,`, inside `()` or `[]`, around `.`, or after unary `-`. `not` is followed by one space. Ranges are tight: `0..n`, `a..=b`. |
| Braces | Struct literals, struct and enum bodies, and struct patterns have inner spaces: `P { x: 1 }`. Map literals do not: `{"a": 1}`. Empty is `{}` everywhere, and an empty struct literal is `P {}`. |
| Blocks | A block is one line if it is empty, or has exactly one statement that is an expression, assignment, `return`, `break`, or `continue`, contains no nested block, `match`, or comment, and the whole line fits. Otherwise it is expanded, one statement per line. `let`, `var`, `while`, and `for` always force expansion. |
| `if` / `else` | `} else {` and `} else if c {` on one line. A one-line `if` may have an `else` only if both branches are one-line blocks and the whole expression fits. |
| `match` | Always expanded, one arm per line. Expression arms end with a comma. Block arms end with `}` and no comma. Guards read `pat if cond =>`. An arm that does not fit breaks after `=>` and indents. An or-pattern that does not fit breaks after a trailing `\|`. |
| Binary chains | If a chain does not fit, every operator at that precedence level breaks, with the continuation indented once. `and`, `or`, and `??` go at the start of the continuation line. All other operators stay at the end of the line. |
| Method chains | The head is the root, any `.field` links before the first call, and the first call (so `editor.text(doc)` and `self.items.last()` stay together). If the whole chain does not fit, every remaining link goes on its own line with a leading `.`, indented once. |
| Last-lambda hug | If the final argument is a lambda and all earlier arguments fit on the opening line, the call is printed `f(a, fn(x) {` … `})` instead of breaking every argument. Only one lambda may hug. |
| Assignment and `let` | If the right side does not fit, break after `=` and indent once, unless the right side is a bracketed or block construct that can open on the same line. |
| Signatures | Parameters wrap as a comma list. The return type stays on the closing line: `) -> Result[(), str] {`. If it still does not fit, break after `->`. |
| `use` | Never wraps. |
| Lambdas | `fn(x) => e` keeps one space around `=>`. If `e` does not fit, break after `=>`. |

Nothing is column-aligned (no aligned `=>`, `:`, or trailing comments). One space precedes a trailing `//` comment.

### 14.4 Blank lines

- Runs of blank lines collapse to one. Leading and trailing blank lines in a block or file are removed, and a file ends with exactly one `\n`.
- Top level: exactly one blank line between items, except that adjacent one-line `const` declarations and adjacent `use` lines keep whatever the author wrote (zero or one).
- Inside `impl`: one blank line between methods, except that adjacent one-line methods keep the author's choice.
- Inside blocks, the author's single blank lines between statements are kept. The formatter never inserts one.
- A `///` or `// allow(...)` comment is glued to what follows it. Blank lines between them are deleted, so doc attachment and suppression cannot be broken by formatting.

### 14.5 Imports

Within each run of consecutive `use` lines (no blank line or non-`use` line between), lines are sorted by path, compared segment by segment by Unicode scalar value, with shorter prefixes first (`app.editor` before `app.editor.Doc`). Aliases do not affect order. Comments directly above a `use` line travel with it. Sorting is safe because items are order-independent. The formatter does not reorder across blank lines, so authors group imports by hand.

### 14.6 Literals and comments

- Strings and interpolations are emitted verbatim.
- Hex digits are uppercase (`0xFF`), prefixes `0x` and `0b` are lowercase, exponent markers are lowercase (`1e9`), and underscores are kept as written.
- `//` and `///` are followed by one space unless the comment is empty or the next character is `/`. `//foo` becomes `// foo`.
- A suppression is normalized to `// allow(L001, L002)`: lowercase `allow`, one space after each comma.
- An own-line comment is indented like the item or statement that follows it, or like the block's statements if it is the last thing in a block. A trailing comment stays on its line. A list or argument group containing a comment is always broken, because a `//` comment cannot sit inside a flat group.

### 14.7 Interface files

`.mti` files are parsed in interface mode and formatted by the same rules. One signature per line, parameters wrap like any `fn`, and a brace-less `struct Name` stays brace-less. Blank-line rules match 14.4. `plugin.toml` and `martite.toml` are not touched by `martite fmt`.

### 14.8 CLI and config

`martite fmt [--check] [--verify] [--stdin] [paths]` formats files in place by default. `--check` prints a unified diff and writes nothing. `--stdin` reads source on stdin and writes the result to stdout. With no paths it walks the project root for `.mt` and `.mti` files. Exit codes follow section 13: 0 if everything is already formatted (or was formatted), 1 if `--check` found a difference or a file had a syntax error, 2 for usage, config, or I/O errors.

```toml
# martite.toml
[fmt]
max_width   = 100      # 40 to 200
indent      = 4        # 2 or 4
end_of_line = "lf"     # "lf" or "crlf"
```

### 14.9 Worked example

Before:

```martite
use app.status
use app.editor

on saved(mut self, doc: Doc, kind: SaveKind) -> Result[(), str] {
  if kind == SaveKind.Auto {
        return Ok(()) }
    let words = editor.text(doc).split(" ").filter(fn(w) => w != "").map(fn(w) => w.trim()).filter(fn(w) => w.len() > 0).len()
    self.saves += 1
    status.show("{words} words after {self.saves} saves")
}
```

After:

```martite
use app.editor
use app.status

on saved(mut self, doc: Doc, kind: SaveKind) -> Result[(), str] {
    if kind == SaveKind.Auto { return Ok(()) }
    let words = editor.text(doc)
        .split(" ")
        .filter(fn(w) => w != "")
        .map(fn(w) => w.trim())
        .filter(fn(w) => w.len() > 0)
        .len()
    self.saves += 1
    status.show("{words} words after {self.saves} saves")
}
```

## Decisions worth challenging

1. **Value semantics with `mut` copy-in/copy-out** instead of references. It is the easiest model to learn and lint, at the cost that large mutation-heavy code leans on copy-on-write performance.
2. **No traits.** Three built-in bounds cover generic containers and sorting, but users can't define their own abstraction over types. `Ord` is derived structurally for every user type, whether or not the author wanted it. User-defined traits are a possible future extension.
3. **Casing as syntax**, which makes patterns trivial but is strict.
4. **A total ban on shadowing**, including globals and prelude names. It is excellent for the linter, and it occasionally forces awkward names. The language edition protects existing code from new prelude names.
5. **Omitted for now:** `?.`, `if let`, ternaries, sets, labeled breaks, format specs, multi-line strings, default arguments, bitwise operators, open-ended ranges (`xs[1..]`, so write `xs[1..xs.len()]`), and range/list patterns. Each has a clean desugaring if you want it back. Also omitted for the embedding use case: `async`/`await` (hooks run to completion), closures passed to the host, plugin-to-plugin calls, and runtime-computed module names.
6. **Host effects are immediate, not transactional.** The `mut self` rollback covers plugin state only, so a handler that calls `editor.replace` and then traps leaves the edit in place. The alternative is for handlers to *return* a list of host commands that the host applies after a successful return. That is atomic and trivially testable, but every host API becomes a command enum, which is heavier for beginners.
7. **Capabilities are static and per module.** There are no runtime permission prompts and no per-path or per-argument grants, so host functions must enforce argument-level policy themselves.
8. **One state value per plugin, addressed as `self`.** It keeps handlers simple, but a plugin with several independent stateful components must bundle them in one struct or split into several plugins.
9. **Fuel is deterministic and work-proportional, not wall-clock.** It is portable, but a cheap node can still be slow if it allocates heavily, which is why `deadline` exists as a second limit (and breaks bit-for-bit replay, so replays should disable it). Host functions are only charged if the embedding charges them.
10. **State migration on reload is minimal.** An unchanged type fingerprint keeps the snapshot, and any change re-runs `init`. A user-written `on migrate(old: …)` is a possible extension. Handles stored in state are only valid within one host session, so a snapshot that holds them restores to stale handles (L021 warns about this).
11. **`Err` commits state.** A handler that returns `Err` after mutating `self` keeps the mutation, because `Err` is a normal return. The alternative (rolling back on `Err`) would make the return type change the meaning of `mut self`.
12. **Strings index by Unicode scalar value.** It matches what beginners expect from "the 3rd character", but `len` and slicing are O(n) on non-ASCII text.
13. **Only free functions are first-class.** Methods and variant constructors need a lambda wrapper, which keeps method resolution static and the checker simple.
14. **Aliases rename only the local binding.** `use a.b as c` has no effect on the declared name, so diagnostics, `Show` output, and snapshots show the original name. The cost is that a reader must map the alias back to see what is imported. The benefit is that name clashes between host modules never block a plugin, and a long module name can be shortened. There are still no glob imports and no re-exports.
15. **One formatter style with three knobs** (`max_width`, `indent`, `end_of_line`). This removes style arguments, at the cost that nobody can adopt a house style.
16. **Single-statement blocks collapse to one line** when they fit. This matches the examples in this document, but adding a second statement changes the first one's layout, so diffs are a little noisier.
17. **Import sorting belongs to the formatter**, not a lint, because it is always safe when items are order-independent. The cost is that `martite fmt` can reorder lines even when it changes nothing else.
18. **`// allow` covers the next element's head, not the next line.** This keeps suppressions valid across reformatting, but it is wider than a single line (a suppression above a multi-line call covers all of it) and narrower than a whole body (above an `if` it does not cover the branches).
19. **`info` diagnostics never fail the build.** That keeps style rules from blocking CI, but they may be ignored forever. Teams that care can raise a group to `warn`.
20. **L026 (panic-capable operations in handlers) is off by default.** In a plugin language every panic is a trap, so it arguably belongs on, but indexing and division are too common for a default-on rule to be useful.
21. **L027 exists because `Err` commits state** (decision 11). If the language ever rolls state back on `Err`, L027 becomes a tombstone.
