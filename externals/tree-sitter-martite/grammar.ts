/// <reference path="./node_modules/tree-sitter-cli/dsl.d.ts" />

/**
 * Tree-sitter grammar for Martite.
 *
 * The Rust lexer/parser in `crates/martite-syntax` is the normative
 * implementation. This grammar exists for editors and other tree-sitter
 * consumers, so it deliberately accepts the union of `.mt` source and `.mti`
 * interface syntax and leaves mode-specific checks to the compiler.
 *
 * This is the source of truth. `scripts/generate-grammar.ts` transpiles it to
 * the checked-in `grammar.js` consumed by the tree-sitter CLI.
 */

const commaSep1 = (rule: RuleOrLiteral): RuleOrLiteral => seq(rule, repeat(seq(',', rule)));

export default grammar({
  name: 'martite',

  extras: ($) => [/\s/, /\uFEFF/, $.comment, $.doc_comment],

  externals: ($) => [
    $._newline,
    $.block_lbrace,
    $.list_lbrace,
    $.map_lbrace,
    $.rbrace,
    $._string_start,
    $._string_end,
    $._interpolation_start,
    $._interpolation_end,
    $.string_content,
    $.escape_sequence,
  ],

  conflicts: ($) => [
    [$.match_arm, $.match_block_arm],
  ],

  supertypes: ($) => [$._item, $._statement, $._base_type, $._pattern_atom],

  word: ($) => $.identifier,

  rules: {
    source_file: ($) =>
      seq(
        repeat($._newline),
        optional(
          seq(
            $._item,
            repeat(seq(repeat1($._newline), $._item)),
            repeat($._newline),
          ),
        ),
      ),

    _item: ($) =>
      choice(
        $.use_declaration,
        $.test_declaration,
        $.on_declaration,
        $.impl_declaration,
        $.function_declaration,
        $.struct_declaration,
        $.opaque_struct_declaration,
        $.enum_declaration,
        $.const_declaration,
      ),

    visibility: (_) => 'pub',

    use_declaration: ($) =>
      seq(
        'use',
        field('path', $.module_path),
        optional(seq('as', field('alias', choice($.identifier, $.type_identifier, $.constant)))),
      ),

    module_path: ($) =>
      seq(
        $.identifier,
        repeat(seq('.', choice($.identifier, $.type_identifier, $.constant))),
      ),

    test_declaration: ($) =>
      seq('test', field('name', $.string), field('body', $.block)),

    on_declaration: ($) =>
      seq(
        'on',
        field('name', $.identifier),
        field('parameters', $.parameter_list),
        optional(seq('->', field('return_type', $.type))),
        optional(field('body', $.block)),
      ),

    impl_declaration: ($) =>
      seq(
        'impl',
        optional(field('type_parameters', $.type_parameters)),
        field('type', $.type_identifier),
        optional(field('type_arguments', $.type_arguments)),
        $.list_lbrace,
        repeat($.function_declaration),
        $.rbrace,
      ),

    function_declaration: ($) =>
      seq(
        optional($.visibility),
        'fn',
        field('name', $.identifier),
        optional(field('type_parameters', $.type_parameters)),
        field('parameters', $.parameter_list),
        optional(seq('->', field('return_type', $.type))),
        optional(field('body', $.block)),
      ),

    parameter_list: ($) =>
      seq('(', optional($._parameters), ')'),

    _parameters: ($) =>
      choice(
        seq($.self_parameter, optional(seq(',', $._normal_parameters))),
        $._normal_parameters,
      ),

    _normal_parameters: ($) =>
      seq($.parameter, repeat(seq(',', $.parameter)), optional(',')),

    self_parameter: ($) => seq(optional('mut'), 'self'),

    parameter: ($) =>
      seq(
        optional('mut'),
        field('name', $.identifier),
        ':',
        field('type', $.type),
      ),

    struct_declaration: ($) =>
      seq(
        optional($.visibility),
        'struct',
        field('name', $.type_identifier),
        optional(field('type_parameters', $.plain_type_parameters)),
        field('body', $.field_declaration_list),
      ),

    opaque_struct_declaration: ($) =>
      seq(
        optional($.visibility),
        'struct',
        field('name', $.type_identifier),
      ),

    field_declaration_list: ($) =>
      seq(
        $.list_lbrace,
        optional(seq($.field_declaration, repeat(seq(',', $.field_declaration)), optional(','))),
        $.rbrace,
      ),

    field_declaration: ($) =>
      seq(
        field('name', $.identifier),
        ':',
        field('type', $.type),
      ),

    enum_declaration: ($) =>
      seq(
        optional($.visibility),
        'enum',
        field('name', $.type_identifier),
        optional(field('type_parameters', $.plain_type_parameters)),
        field('body', $.variant_list),
      ),

    variant_list: ($) =>
      seq(
        $.list_lbrace,
        optional(seq($.variant, repeat(seq(',', $.variant)), optional(','))),
        $.rbrace,
      ),

    variant: ($) =>
      seq(
        field('name', $.type_identifier),
        optional($.variant_payload),
      ),

    variant_payload: ($) =>
      seq('(', optional(seq(commaSep1($.type), optional(','))), ')'),

    const_declaration: ($) =>
      seq(
        optional($.visibility),
        'const',
        field('name', $.constant),
        ':',
        field('type', $.type),
        '=',
        field('value', $._expression),
      ),

    plain_type_parameters: ($) =>
      seq('[', optional(seq(commaSep1($.type_identifier), optional(','))), ']'),

    type_parameters: ($) =>
      seq('[', optional(seq(commaSep1($.type_parameter), optional(','))), ']'),

    type_parameter: ($) =>
      seq(
        field('name', $.type_identifier),
        optional(seq(':', field('bounds', $.bounds))),
      ),

    bounds: ($) =>
      seq($.bound, repeat(seq('+', $.bound))),

    bound: (_) => choice('Eq', 'Ord', 'Show'),

    // ----- types --------------------------------------------------------

    type: ($) => choice($._base_type, $.optional_type),

    optional_type: ($) =>
      prec(1, seq(field('type', $._base_type), '?')),

    _base_type: ($) =>
      choice(
        $.primitive_type,
        $.unit_type,
        $.tuple_type,
        $.parenthesized_type,
        $.list_type,
        $.map_type,
        $.named_type,
        $.function_type,
      ),

    primitive_type: (_) => choice('int', 'float', 'bool', 'str', 'range'),

    named_type: ($) =>
      seq(
        field('name', $.type_identifier),
        optional(field('type_arguments', $.type_arguments)),
      ),

    list_type: ($) =>
      seq('list', field('type_arguments', $.type_arguments)),

    map_type: ($) =>
      seq('map', field('type_arguments', $.type_arguments)),

    type_arguments: ($) =>
      seq('[', optional(seq(commaSep1($.type), optional(','))), ']'),

    unit_type: ($) => seq('(', ')'),

    tuple_type: ($) =>
      seq(
        '(',
        field('element', $.type),
        ',',
        optional(seq(commaSep1($.type), optional(','))),
        ')',
      ),

    parenthesized_type: ($) =>
      seq('(', field('type', $.type), ')'),

    function_type: ($) =>
      seq(
        'fn',
        '(',
        optional(seq(commaSep1($.type), optional(','))),
        ')',
        optional(seq('->', field('return_type', $.type))),
      ),

    // ----- statements ---------------------------------------------------

    block: ($) =>
      prec(
        1,
        seq(
          $.block_lbrace,
          repeat($._newline),
          optional(
            seq(
              $._statement,
              repeat(seq(repeat1($._newline), $._statement)),
              repeat($._newline),
            ),
          ),
          $.rbrace,
        ),
      ),

    _statement: ($) =>
      choice(
        $.let_statement,
        $.var_statement,
        $.while_statement,
        $.for_statement,
        $.return_statement,
        $.break_statement,
        $.continue_statement,
        $.expression_statement,
      ),

    let_statement: ($) =>
      seq(
        'let',
        field('pattern', $.pattern),
        optional(seq(':', field('type', $.type))),
        '=',
        field('value', $._expression),
      ),

    var_statement: ($) =>
      seq(
        'var',
        field('pattern', $.pattern),
        optional(seq(':', field('type', $.type))),
        '=',
        field('value', $._expression),
      ),

    while_statement: ($) =>
      seq(
        'while',
        field('condition', $._expression),
        optional($._newline),
        field('body', $.block),
      ),

    for_statement: ($) =>
      seq(
        'for',
        field('pattern', $.pattern),
        'in',
        field('value', $._expression),
        optional($._newline),
        field('body', $.block),
      ),

    return_statement: ($) =>
      seq('return', optional(field('value', $._expression))),

    break_statement: (_) => 'break',

    continue_statement: (_) => 'continue',

    expression_statement: ($) =>
      seq(
        field('expression', $._expression),
        optional(
          seq(
            field('operator', choice('=', '+=', '-=', '*=', '/=', '%=')),
            field('value', $._expression),
          ),
        ),
      ),

    // ----- expressions --------------------------------------------------

    _expression: ($) => choice($._or_expression, $.coalesce_expression),

    coalesce_expression: ($) =>
      prec.right(
        1,
        seq(
          field('left', $._or_expression),
          '??',
          field('right', $._expression),
        ),
      ),

    _or_expression: ($) =>
      choice(
        $._and_expression,
        $.or_expression,
      ),

    or_expression: ($) =>
      prec.left(
        2,
        seq(
          field('left', $._or_expression),
          'or',
          field('right', $._and_expression),
        ),
      ),

    _and_expression: ($) =>
      choice(
        $._not_expression,
        $.and_expression,
      ),

    and_expression: ($) =>
      prec.left(
        3,
        seq(
          field('left', $._and_expression),
          'and',
          field('right', $._not_expression),
        ),
      ),

    _not_expression: ($) =>
      choice(
        $._comparison_expression,
        $.not_expression,
      ),

    not_expression: ($) =>
      prec.right(
        4,
        seq('not', field('operand', $._not_expression)),
      ),

    _comparison_expression: ($) =>
      choice(
        $._range_expression,
        $.comparison_expression,
      ),

    comparison_expression: ($) =>
      prec(
        5,
        seq(
          field('left', $._range_expression),
          field('operator', choice('==', '!=', '<', '<=', '>', '>=')),
          field('right', $._range_expression),
        ),
      ),

    _range_expression: ($) =>
      choice(
        $._additive_expression,
        $.range_expression,
      ),

    range_expression: ($) =>
      prec(
        6,
        seq(
          field('start', $._additive_expression),
          field('operator', choice('..', '..=')),
          field('end', $._additive_expression),
        ),
      ),

    _additive_expression: ($) =>
      choice(
        $._multiplicative_expression,
        $.additive_expression,
      ),

    additive_expression: ($) =>
      prec.left(
        7,
        seq(
          field('left', $._additive_expression),
          field('operator', choice('+', '-')),
          field('right', $._multiplicative_expression),
        ),
      ),

    _multiplicative_expression: ($) =>
      choice(
        $._unary_expression,
        $.multiplicative_expression,
      ),

    multiplicative_expression: ($) =>
      prec.left(
        8,
        seq(
          field('left', $._multiplicative_expression),
          field('operator', choice('*', '/', '%')),
          field('right', $._unary_expression),
        ),
      ),

    _unary_expression: ($) =>
      choice(
        $._postfix_expression,
        $.unary_expression,
      ),

    unary_expression: ($) =>
      prec(9, seq('-', field('operand', $._unary_expression))),

    _postfix_expression: ($) =>
      choice(
        $._primary_expression,
        $.call_expression,
        $.index_expression,
        $.field_expression,
        $.try_expression,
      ),

    call_expression: ($) =>
      prec.left(
        10,
        seq(
          field('function', $._postfix_expression),
          field('arguments', $.arguments),
        ),
      ),

    arguments: ($) =>
      seq('(', optional(seq(commaSep1($.argument), optional(','))), ')'),

    argument: ($) =>
      seq(optional('mut'), field('value', $._expression)),

    index_expression: ($) =>
      prec.left(
        10,
        seq(
          field('base', $._postfix_expression),
          '[',
          field('index', $._expression),
          ']',
        ),
      ),

    field_expression: ($) =>
      prec.left(
        10,
        seq(
          field('base', $._postfix_expression),
          '.',
          field(
            'field',
            choice($.identifier, $.type_identifier, $.constant, $.integer_literal),
          ),
        ),
      ),

    try_expression: ($) =>
      prec.left(10, seq(field('operand', $._postfix_expression), '?')),

    _primary_expression: ($) =>
      choice(
        $.integer_literal,
        $.float_literal,
        $.string,
        $.boolean_literal,
        $.unit_expression,
        $.self_expression,
        $.identifier,
        $.constant,
        $.type_identifier,
        $.struct_literal,
        $.conversion_expression,
        $.tuple_expression,
        $.list_literal,
        $.map_literal,
        $.parenthesized_expression,
        $.if_expression,
        $.match_expression,
        $.lambda_expression,
      ),

    integer_literal: (_) =>
      token(
        choice(
          /0/,
          /[1-9][0-9]*(?:_[0-9]+)*/,
          /0[xX][0-9a-fA-F]+(?:_[0-9a-fA-F]+)*/,
          /0[bB][01]+(?:_[01]+)*/,
        ),
      ),

    float_literal: (_) =>
      token(
        choice(
          /(?:0|[1-9][0-9]*(?:_[0-9]+)*)\.[0-9]+(?:_[0-9]+)*(?:[eE][+-]?[0-9]+(?:_[0-9]+)*)?/,
          /(?:0|[1-9][0-9]*(?:_[0-9]+)*)[eE][+-]?[0-9]+(?:_[0-9]+)*/,
        ),
      ),

    boolean_literal: (_) => choice('true', 'false'),

    self_expression: (_) => 'self',

    string: ($) =>
      seq(
        $._string_start,
        repeat(choice($.string_content, $.escape_sequence, $.interpolation)),
        $._string_end,
      ),

    interpolation: ($) =>
      seq(
        $._interpolation_start,
        field('value', $._expression),
        $._interpolation_end,
      ),

    unit_expression: ($) => seq('(', ')'),

    parenthesized_expression: ($) =>
      seq('(', field('value', $._expression), ')'),

    tuple_expression: ($) =>
      seq(
        '(',
        field('element', $._expression),
        ',',
        optional(seq(commaSep1($._expression), optional(','))),
        ')',
      ),

    list_literal: ($) =>
      seq('[', optional(seq(commaSep1($._expression), optional(','))), ']'),

    map_literal: ($) =>
      seq(
        $.map_lbrace,
        optional(seq($.map_entry, repeat(seq(',', $.map_entry)), optional(','))),
        $.rbrace,
      ),

    map_entry: ($) =>
      seq(
        field('key', $._expression),
        ':',
        field('value', $._expression),
      ),

    struct_literal: ($) =>
      prec(
        11,
        seq(
          field('name', $.type_identifier),
          $.list_lbrace,
          optional($._struct_literal_body),
          $.rbrace,
        ),
      ),

    _struct_literal_body: ($) =>
      choice(
        seq(
          '..',
          field('base', $._expression),
          optional(seq(',', optional($._field_initializer_list))),
        ),
        $._field_initializer_list,
      ),

    _field_initializer_list: ($) =>
      seq($.field_initializer, repeat(seq(',', $.field_initializer)), optional(',')),

    field_initializer: ($) =>
      seq(
        field('name', $.identifier),
        optional(seq(':', field('value', $._expression))),
      ),

    conversion_expression: ($) =>
      seq(
        field('type', choice('int', 'float', 'str')),
        '(',
        field('value', $._expression),
        ')',
      ),

    if_expression: ($) =>
      seq(
        'if',
        field('condition', $._expression),
        optional($._newline),
        field('consequence', $.block),
        optional(
          seq(
            'else',
            field('alternative', choice($.block, $.if_expression)),
          ),
        ),
      ),

    match_expression: ($) =>
      seq(
        'match',
        field('value', $._expression),
        $.list_lbrace,
        repeat(choice(seq($.match_arm, ','), $.match_block_arm)),
        optional($.match_arm),
        $.rbrace,
      ),

    match_arm: ($) =>
      seq(
        field('pattern', $.pattern),
        optional(seq('if', field('guard', $._expression))),
        '=>',
        field('body', choice($.block, $._expression)),
      ),

    match_block_arm: ($) =>
      seq(
        field('pattern', $.pattern),
        optional(seq('if', field('guard', $._expression))),
        '=>',
        field('body', $.block),
      ),

    lambda_expression: ($) =>
      seq(
        'fn',
        '(',
        optional(seq(commaSep1($.lambda_parameter), optional(','))),
        ')',
        optional(seq('->', field('return_type', $.type))),
        field('body', choice($.block, seq('=>', $._expression))),
      ),

    lambda_parameter: ($) =>
      seq(
        field('pattern', $.pattern),
        optional(seq(':', field('type', $.type))),
      ),

    // ----- patterns -----------------------------------------------------

    pattern: ($) =>
      choice($._pattern_atom, $.or_pattern),

    or_pattern: ($) =>
      prec.left(seq($._pattern_atom, '|', field('pattern', $.pattern))),

    _pattern_atom: ($) =>
      choice(
        $.wildcard_pattern,
        $.literal_pattern,
        $.binding_pattern,
        $.variant_pattern,
        $.struct_pattern,
        $.unit_pattern,
        $.tuple_pattern,
        $.parenthesized_pattern,
      ),

    wildcard_pattern: (_) => '_',

    literal_pattern: ($) =>
      choice(
        $.integer_literal,
        seq('-', field('value', $.integer_literal)),
        $.string,
        $.boolean_literal,
      ),

    binding_pattern: ($) =>
      field('name', $.identifier),

    variant_pattern: ($) =>
      seq(
        choice(
          seq(
            field('qualifier', $.type_identifier),
            '.',
            field('name', $.type_identifier),
          ),
          field('name', $.type_identifier),
        ),
        optional(field('arguments', $.pattern_arguments)),
      ),

    pattern_arguments: ($) =>
      seq('(', optional(seq(commaSep1($.pattern), optional(','))), ')'),

    struct_pattern: ($) =>
      seq(
        field('type', $.type_identifier),
        $.list_lbrace,
        optional($._struct_pattern_body),
        $.rbrace,
      ),

    _struct_pattern_body: ($) =>
      choice(
        seq($.rest_pattern, optional(',')),
        seq(
          $.field_pattern,
          repeat(seq(',', $.field_pattern)),
          optional(seq(',', $.rest_pattern)),
          optional(','),
        ),
      ),

    field_pattern: ($) =>
      seq(
        field('name', $.identifier),
        optional(seq(':', field('pattern', $.pattern))),
      ),

    rest_pattern: (_) => '..',

    unit_pattern: ($) => seq('(', ')'),

    tuple_pattern: ($) =>
      seq(
        '(',
        field('element', $.pattern),
        ',',
        optional(seq(commaSep1($.pattern), optional(','))),
        ')',
      ),

    parenthesized_pattern: ($) =>
      seq('(', field('pattern', $.pattern), ')'),

    // ----- lexical helpers ----------------------------------------------

    identifier: (_) => /_?[a-z][a-z0-9_]*/,

    type_identifier: (_) => /[A-Z][A-Za-z0-9]*/,

    constant: (_) => token(prec(1, /[A-Z][A-Z0-9_]+/)),

    comment: (_) => token(seq('//', /[^\n]*/)),

    doc_comment: (_) => token(prec(1, seq('///', /[^\n]*/))),
  },
});
