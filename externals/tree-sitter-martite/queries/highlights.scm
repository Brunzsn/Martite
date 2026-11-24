(identifier) @variable

; Keywords
[
  "fn"
  "let"
  "var"
  "const"
  "struct"
  "enum"
  "impl"
  "use"
  "as"
  "test"
  "on"
  "if"
  "else"
  "match"
  "while"
  "for"
  "in"
  "return"
  "mut"
  "and"
  "or"
  "not"
  "true"
  "false"
] @keyword

(visibility) @keyword
(break_statement) @keyword
(continue_statement) @keyword

(self_expression) @variable.builtin
(self_parameter) @variable.builtin

; Comments
(comment) @comment
(doc_comment) @comment.documentation

; Literals
(integer_literal) @number
(float_literal) @number
(boolean_literal) @boolean
(string) @string
(escape_sequence) @string.escape
(string_content) @string
(interpolation) @embedded

; Types
(primitive_type) @type.builtin
(named_type name: (type_identifier) @type)
(type_identifier) @type
(constant) @constant

; Declarations
(function_declaration name: (identifier) @function)
(on_declaration name: (identifier) @function.method)
(struct_declaration name: (type_identifier) @type)
(enum_declaration name: (type_identifier) @type)
(variant name: (type_identifier) @constructor)
(field_declaration name: (identifier) @variable.member)
(field_initializer name: (identifier) @variable.member)
(field_pattern name: (identifier) @variable.member)
(field_expression field: (identifier) @variable.member)
(variant_pattern name: (type_identifier) @constructor)
(variant_pattern qualifier: (type_identifier) @type)
(struct_pattern type: (type_identifier) @type)
(struct_literal name: (type_identifier) @type)
(binding_pattern name: (identifier) @variable.parameter)
(parameter name: (identifier) @variable.parameter)

; Functions and method calls
(call_expression function: (identifier) @function.call)
(field_expression field: (identifier) @function.method.call)

; Patterns
(wildcard_pattern) @variable.builtin
(type_parameter name: (type_identifier) @type.parameter)
(bound) @type.builtin

; Operators
[
  "+"
  "-"
  "*"
  "/"
  "%"
  "=="
  "!="
  "<"
  "<="
  ">"
  ">="
  "="
  "+="
  "-="
  "*="
  "/="
  "%="
  ".."
  "..="
  "??"
  "?"
  "=>"
  "->"
  "|"
] @operator

; Punctuation
["," ":" "."] @punctuation.delimiter
