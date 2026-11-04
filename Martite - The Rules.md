# Martite: The Rules

*Status: spec-freeze candidate, language edition 1. Sections 0 to 10 and 13 are normative. Section 11 (embedding API) is an illustrative sketch. Section 12 fixes lint IDs and meanings, but not severities.*

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
| `Eq`, `Ord`, and `Show` are structural, and the `Show` text format is specified | Interpolation, `print`, and sorting behave identically in every implementation |

## 0. Files, modules, editions, and load limits

- **File types:** `.mt` source, `.mti` host interface, `plugin.toml` plugin manifest, `martite.toml` tool configuration (lint thresholds, severities, formatter options).
- **Encoding:** UTF-8. A leading BOM is ignored. `\r\n` is normalized to `\n` before lexing. Tabs are legal whitespace. A NUL byte is a lexical error.
- **Modules:** a file is a module. The **project root** is the directory of the entry file (the manifest's `entry` for a plugin, the file given to `martite run` for a script). `use geometry.shapes` loads `<root>/geometry/shapes.mt`. `std.*` is built in. Host modules come from interface files (`app/editor.mti` is module `app.editor`). A module path that matches both a source file and an interface is E106.
- **The interpreter never touches the filesystem.** It asks a `SourceProvider` for the text of a module path. The CLI supplies a filesystem provider, and an embedding supplies whatever it likes (an in-memory map, an archive).
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
- **Comments:** `//` and `///`. A `///` directly before an item is its doc comment, and anywhere else it is an ordinary comment. Comments are kept as trivia on tokens so the linter and formatter can see them.
- **Suppression:** `// allow(L001)` or `// allow(L001, L002)` on the line above a diagnostic suppresses those lint IDs for diagnostics whose primary span starts on the next line. Compile errors (`E…`) cannot be suppressed.
- **Newlines** end statements. A newline is ignored when:
  - the previous token is a binary operator, an assignment operator, `,`, `:`, `=>`, `->`, or an opening bracket;
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

- A body-less `fn` is a **host function**. It is an ordinary typed function (it may be generic), runs synchronously, and may have effects on the host. It can fail only by returning `Result` or `Option`. A fault inside the Rust implementation itself (a bug) surfaces as the `HostFault` trap.
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

- **Loading fails (E508)** if the manifest is malformed or has unknown keys, `lang` is unsupported, `api` has a different major version from the host's interface, `api`'s minor is greater than the host's, the entry file is missing, or a granted module does not exist. Adding hooks, functions, or types is a compatible change (minor bump). Removing or retyping one is not (major bump).
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
- **Panics** are uncatchable inside the language. They are index out of bounds, invalid slice, divide by zero, overflow, invalid `int(f)`, `expect` on `None`/`Err`, failed `assert`, and explicit `panic(msg)`. A script prints `panic: <msg> at <file>:<line>:<col>` to stderr and exits with status 101 (an `Err` from `main` exits with 1). A plugin hook **traps** (below). There are no exceptions; recoverable errors are `Result` values.
- **`for`** iterates `list[T]` (yields `T`), `map[K,V]` (yields `(K,V)`), `str` (yields 1-char `str`), and `range` (yields `int`).
- **`while true`** is the only infinite loop, and there is no `loop`.

**Traps and limits.** A trap aborts the current hook, discards its `mut self` copy, and returns the trap to the host. The instance stays loaded with its pre-call state, and the host decides policy (log, retry, or disable after repeated traps).

| Trap | Cause |
| --- | --- |
| `Panic(msg, span, trace)` | any panic listed above, with a call trace |
| `OutOfFuel` | the step budget ran out |
| `OutOfMemory` | live value bytes exceeded the cap |
| `DepthExceeded` | call depth or expression nesting exceeded the cap |
| `Deadline` | the wall-clock deadline passed (checked every N fuel ticks and at host calls) |
| `HostFault(msg)` | a host function failed in a way its signature cannot express (a bug in the host) |

- **Limits** are per hook call and set by the host as `Limits { fuel, memory, depth, deadline }`. The manifest may request limits, and the host clamps them. The engine may also enforce a compiled-in depth ceiling so that the interpreter's native stack cannot overflow.
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
| `float` | `abs`, `floor`, `ceil`, `trunc`, `sqrt`, `pow(e: float)`, `round(digits: int)` (round half away from zero at that decimal place, `digits >= 0` else panic), `is_nan`, `is_finite` |
| `range` | `len() -> int`, `contains(i: int) -> bool`, `to_list() -> list[int]` |
| `Option[T]` | `is_some`, `is_none`, `map(f)`, `and_then(f)`, `unwrap_or(d)`, `ok_or(e) -> Result[T, E]`, `expect(msg)` |
| `Result[T,E]` | `is_ok`, `is_err`, `map(f)`, `map_err(f)`, `and_then(f)`, `unwrap_or(d)`, `ok() -> T?`, `expect(msg)` |

**Edge cases (normative):**

- `split(sep)` splits on non-overlapping occurrences and keeps empty pieces, so `"".split(",")` is `[""]`, and `split("")` splits into characters. `replace` with an empty `from` panics. `trim` removes Unicode whitespace at both ends.
- `parse_int` accepts `-?[0-9]+` only (no whitespace, no `+`, no underscores) and returns `None` on overflow. `parse_float` accepts decimal and exponent forms, and returns `None` for anything else, including `NaN` and `inf`.
- `unwrap_or(d)` evaluates `d` eagerly. Use `??` for a lazy default.
- `map`, `filter`, `enumerate`, `reversed`, `sorted`, `keys`, `values`, and `entries` are eager and return new lists.
- Indexing `xs[i]` and `m[k]` panics if missing, and `.get` is the safe form. `martite lint` flags unguarded `expect`.
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
- `.mti` files are parsed in interface mode, where `fn` and `on` signatures have no body and a brace-less `struct Name` is an opaque handle. A newline ends each signature.
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
- **Snapshots** are a canonical, self-describing encoding of the state value together with a **type fingerprint**: a hash over the names and shapes of every type reachable from the state type (field names and types, variant names and order, type parameters), ignoring doc comments and `pub`. Opaque handles are encoded as host-defined tokens tagged as valid for one host session only.
- **Reload.** The host re-checks the new source and compares fingerprints. If they are equal it restores the snapshot without running `init`; otherwise it re-runs `init` (the host may instead choose to refuse the reload).
- **Test doubles.** The embedding can ship a test harness that registers fake host functions and supplies handles, so `test` blocks in plugins run offline.

## 12. Linter design

The linter is a set of rules over one immutable analysis result.

**Contracts the Rust core should guarantee:**

- Every AST node has a `NodeId` and a `Span`, and tokens carry comment trivia.
- Resolver and checker output live in side tables keyed by `NodeId`: binding for each identifier, type for each expression, use and assignment counts per binding.
- A rule is `fn check(&Ctx, &mut Emit)`, and rules never mutate the tree. A fix is a list of `Edit { span, replacement }`, which `--fix` can apply safely because spans are exact.
- Rules are tiered by the analysis they need (syntax only, then resolved, then typed), so the first rules can be written before the checker exists.
- Output is deterministic and sorted by span.
- The linter also reads `.mti` files and `plugin.toml` as inputs, so host-aware rules work offline, without the host.

**Starter rules (default severity):**

| ID | Rule | Needs |
| --- | --- | --- |
| L001 | Unused binding, parameter, private item, or import (`_` prefix opts out) | resolved |
| L002 | `var` never reassigned: use `let` (fix) | resolved |
| L003 | Unreachable or redundant match arm | patterns |
| L004 | Unreachable statement after `return`, `break`, `continue`, `panic`, or a `while true` with no `break` | syntax |
| L005 | Constant condition (`if true`), except `while true` | syntax |
| L006 | Self-comparison or self-assignment (`x == x`, `x = x`) | resolved |
| L007 | `==`/`!=` on floats | typed |
| L008 | `expect` outside tests (in a handler it traps the whole hook; prefer returning `Err`) | typed |
| L009 | `let _ =` discarding a `Result` | typed |
| L010 | Integer division or remainder by literal zero | syntax |
| L011 | `x == true`, `if c { true } else { false }`, `not (a == b)` (fix) | syntax |
| L012 | `x = x + n` to `x += n` (fix) | resolved |
| L013 | `return` as the final statement, or `else` after a returning `if` (fix) | syntax |
| L014 | `match` on a `bool` to `if` | typed |
| L015 | `mut` parameter or `mut self` never mutated | resolved |
| L016 | Empty block | syntax |
| L017 | Function length, parameter count, nesting depth, or branch count over threshold | syntax |
| L018 | `pub` item without `///` doc | syntax + trivia |
| L019 | `while true` inside a handler with no reachable `break` or `return` (it can only end in `OutOfFuel`) | syntax |
| L020 | Manifest grants a module that no file of the plugin `use`s (least privilege) | resolved + manifest |
| L021 | An opaque host handle stored in the plugin state type (it is stale after a restart or restore) | typed |

Thresholds and severities live in `martite.toml`. Because the compiler already rejects shadowing, casing errors, type errors, and non-exhaustive matches, the lint set is almost entirely about simplification and suspicious code.

## 13. Tooling and conformance

- **Commands:** `martite run <file>` (script), `martite check <path>` (parse and type-check; with a plugin directory it also reads `plugin.toml` and the interfaces given by `--interface`), `martite lint [--fix] [--json]`, `martite test`, and `martite fmt`. The formatter's canonical style (4 spaces, 100 columns, trailing commas in multi-line lists) is configured in `martite.toml` and is not otherwise part of this document.
- **Test discovery:** `martite test <path>` loads the entry module and every module reachable through `use`, then runs every `test` block in that program. There is no name filter. It supplies no host test double, so a test that calls a host function traps. The command exits 1 if any test fails.
- **Exit codes:** 0 success, 1 diagnostics found, a test failed, or `main` returned `Err`, 2 usage or load error, 101 panic or any other trap in a script.
- **JSON diagnostics** (`--json`) are one object per diagnostic: `code`, `severity`, `message`, `file`, `span` (`start` and `end` byte offsets and 1-based `line`/`column`, where a column counts Unicode scalar values), optional `help`, and optional `fix` (a list of `{span, replacement}` edits).
- **Conformance suite.** The reference implementation is considered complete only when it ships, and passes: at least one failing and one passing fixture for every E-code and every lint rule; golden tests for the `Show` format (section 9.2); golden tests for the fuel and memory tables (section 8); grammar fixtures for every parser note in section 10; and a fuzz run over the lexer, parser, and checker confirming that no input panics the host or exceeds the load limits.

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
