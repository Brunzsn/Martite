/**
 * Semantic query metadata for the Martite tree-sitter grammar.
 *
 * Everything structural (node names, fields, literal tokens) is validated
 * against `src/grammar.json` and `src/node-types.json` by the generator. This
 * file only declares the facts the grammar cannot express: which literals are
 * keywords, which names belong to the prelude, and which Zed features should
 * be generated.
 */

export const KEYWORDS = [
  'fn',
  'let',
  'var',
  'const',
  'struct',
  'enum',
  'impl',
  'use',
  'as',
  'pub',
  'test',
  'on',
  'if',
  'else',
  'match',
  'while',
  'for',
  'in',
  'return',
  'break',
  'continue',
  'mut',
  'self',
  'and',
  'or',
  'not',
  'true',
  'false',
] as const;

/**
 * Keywords whose literal token is wrapped in a named node. The generated
 * keyword group excludes these so the node can use a more precise capture.
 */
export const KEYWORD_NODE_COVERAGE: Record<string, string[]> = {
  break: ['break_statement'],
  continue: ['continue_statement'],
  false: ['boolean_literal'],
  pub: ['visibility'],
  self: ['self_expression', 'self_parameter'],
  true: ['boolean_literal'],
};

export const CONTEXTUAL_ON_NAMES = ['init', 'shutdown'];

export const RESERVED_TYPES = ['int', 'float', 'bool', 'str', 'list', 'map', 'range'];

export const PRELUDE_TYPES = ['Option', 'Result'];
export const PRELUDE_VARIANTS = ['Some', 'None', 'Ok', 'Err'];
export const PRELUDE_FUNCTIONS = ['print', 'panic', 'assert'];
export const PRELUDE_BOUNDS = ['Eq', 'Ord', 'Show'];

export const OPERATORS = [
  '+',
  '-',
  '*',
  '/',
  '%',
  '==',
  '!=',
  '<',
  '<=',
  '>',
  '>=',
  '=',
  '+=',
  '-=',
  '*=',
  '/=',
  '%=',
  '..',
  '..=',
  '??',
  '?',
  '=>',
  '->',
  '|',
];

export const BRACKET_TOKENS = {
  anonymous: [
    ['(', ')'],
    ['[', ']'],
  ],
  named: [
    ['block_lbrace', 'rbrace'],
    ['list_lbrace', 'rbrace'],
    ['map_lbrace', 'rbrace'],
  ],
} as const;

/** Nodes that are intentionally not mentioned by any generated query. */
export const IGNORED_NODES = new Set([
  '_base_type',
  '_item',
  '_pattern_atom',
  '_statement',
  'additive_expression',
  'and_expression',
  'argument',
  'arguments',
  'binding_pattern',
  'bounds',
  'coalesce_expression',
  'comparison_expression',
  'conversion_expression',
  'expression_statement',
  'field_declaration_list',
  'function_type',
  'index_expression',
  'lambda_parameter',
  'list_literal',
  'list_type',
  'literal_pattern',
  'map_entry',
  'map_literal',
  'map_type',
  'match_arm',
  'match_block_arm',
  'match_expression',
  'module_path',
  'multiplicative_expression',
  'not_expression',
  'optional_type',
  'or_expression',
  'or_pattern',
  'parenthesized_expression',
  'parenthesized_pattern',
  'parenthesized_type',
  'parameter_list',
  'pattern',
  'pattern_arguments',
  'plain_type_parameters',
  'primitive_type',
  'range_expression',
  'rest_pattern',
  'return_statement',
  'source_file',
  'struct_pattern',
  'try_expression',
  'tuple_expression',
  'tuple_pattern',
  'tuple_type',
  'type',
  'type_arguments',
  'type_parameters',
  'unary_expression',
  'unit_expression',
  'unit_pattern',
  'unit_type',
  'use_declaration',
  'variant_list',
  'variant_payload',
  'while_statement',
  'for_statement',
  'let_statement',
  'var_statement',
]);

export const OUTLINE_NODES = [
  'function_declaration',
  'on_declaration',
  'struct_declaration',
  'opaque_struct_declaration',
  'enum_declaration',
  'impl_declaration',
  'variant',
  'const_declaration',
  'test_declaration',
];

export const OUTLINE_NAME_FIELDS: Record<string, string> = {
  impl_declaration: 'type',
  test_declaration: 'name',
};

export const FUNCTION_NODES = ['function_declaration', 'on_declaration', 'test_declaration', 'lambda_expression'];
export const CLASS_NODES = [
  'struct_declaration',
  'enum_declaration',
  'impl_declaration',
  'opaque_struct_declaration',
];

export const RUNNABLES = [
  {
    tag: 'martite-main',
    query: `(
  (function_declaration name: (identifier) @run)
  (#eq? @run "main")
  (#set! tag martite-main)
)`,
  },
  {
    tag: 'martite-test',
    query: `(
  (test_declaration name: (string (string_content) @run))
  (#set! tag martite-test)
)`,
  },
];

export interface Task {
  label: string;
  command: string;
  args: string[];
  tags?: string[];
}

export const TASKS: Task[] = [
  { label: 'martite: check', command: 'martite', args: ['check', '$ZED_FILE'] },
  { label: 'martite: lint', command: 'martite', args: ['lint', '$ZED_FILE'] },
  { label: 'martite: run', command: 'martite', args: ['run', '$ZED_FILE'], tags: ['martite-main'] },
  { label: 'martite: test', command: 'martite', args: ['test', '$ZED_FILE'], tags: ['martite-test'] },
  { label: 'martite: fmt', command: 'martite', args: ['fmt', '$ZED_FILE'] },
];

/** Captures Zed recognises for highlighting. */
export const ZED_HIGHLIGHT_CAPTURES = new Set([
  'attribute',
  'boolean',
  'comment',
  'comment.doc',
  'constant',
  'constant.builtin',
  'constructor',
  'embedded',
  'emphasis',
  'emphasis.strong',
  'enum',
  'function',
  'hint',
  'keyword',
  'label',
  'link_text',
  'link_uri',
  'number',
  'operator',
  'predictive',
  'preproc',
  'primary',
  'property',
  'punctuation',
  'punctuation.bracket',
  'punctuation.delimiter',
  'punctuation.list_marker',
  'punctuation.special',
  'string',
  'string.escape',
  'string.regex',
  'string.special',
  'string.special.symbol',
  'tag',
  'tag.doctype',
  'text.literal',
  'title',
  'type',
  'type.builtin',
  'variable',
  'variable.special',
  'variable.parameter',
  'variant',
]);

/** Non-highlight captures used by Zed's structural query files. */
export const ZED_FEATURE_CAPTURES = new Set([
  'open',
  'close',
  'indent',
  'start',
  'end',
  'outdent',
  'start.if',
  'item',
  'name',
  'context',
  'context.extra',
  'annotation',
  'function.around',
  'function.inside',
  'class.around',
  'class.inside',
  'comment.around',
  'comment.inside',
  'run',
  '_name',
]);
