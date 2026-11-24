import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import {
  BRACKET_TOKENS,
  CLASS_NODES,
  CONTEXTUAL_ON_NAMES,
  FUNCTION_NODES,
  IGNORED_NODES,
  KEYWORDS,
  KEYWORD_NODE_COVERAGE,
  OPERATORS,
  OUTLINE_NAME_FIELDS,
  OUTLINE_NODES,
  PRELUDE_BOUNDS,
  PRELUDE_FUNCTIONS,
  PRELUDE_TYPES,
  PRELUDE_VARIANTS,
  RESERVED_TYPES,
  RUNNABLES,
  TASKS,
  ZED_FEATURE_CAPTURES,
  ZED_HIGHLIGHT_CAPTURES,
} from '../query-spec';
import { grammarDir } from './tree-sitter';

interface NodeField {
  multiple: boolean;
  required: boolean;
  types: { type: string; named: boolean }[];
}

interface NodeTypeEntry {
  type: string;
  named: boolean;
  fields?: Record<string, NodeField>;
}

interface GrammarJson {
  externals: { type: string; name: string }[];
  rules: Record<string, unknown>;
}

const grammar = JSON.parse(readFileSync(join(grammarDir, 'src', 'grammar.json'), 'utf8')) as GrammarJson;
const nodeTypes = JSON.parse(
  readFileSync(join(grammarDir, 'src', 'node-types.json'), 'utf8'),
) as NodeTypeEntry[];

const nodeMap = new Map(nodeTypes.filter((node) => node.named).map((node) => [node.type, node]));
const literalSet = new Set<string>();

function walkRule(rule: unknown): void {
  if (rule === null || typeof rule !== 'object') {
    return;
  }
  if (Array.isArray(rule)) {
    for (const item of rule) {
      walkRule(item);
    }
    return;
  }
  const record = rule as Record<string, unknown>;
  if (record.type === 'STRING' && typeof record.value === 'string') {
    literalSet.add(record.value);
  }
  for (const value of Object.values(record)) {
    walkRule(value);
  }
}

for (const rule of Object.values(grammar.rules)) {
  walkRule(rule);
}

function fail(message: string): never {
  console.error(message);
  process.exit(1);
}

function requireNode(name: string): NodeTypeEntry {
  const node = nodeMap.get(name);
  if (!node) {
    fail(`query-spec references unknown node type: ${name}`);
  }
  return node;
}

function requireField(name: string, fieldName: string): NodeField {
  const node = requireNode(name);
  const field = node.fields?.[fieldName];
  if (!field) {
    fail(`query-spec references unknown field ${name}.${fieldName}`);
  }
  return field;
}

function requireLiteral(value: string): void {
  if (!literalSet.has(value)) {
    fail(`query-spec references a literal that is not in the grammar: ${JSON.stringify(value)}`);
  }
}

function quote(value: string): string {
  return JSON.stringify(value);
}

function literalGroup(values: string[], capture: string): string {
  return `[\n${values.map((value) => `  ${quote(value)}`).join('\n')}\n] @${capture}`;
}

function publicNode(name: string): string {
  return name.startsWith('_') ? name.slice(1) : name;
}

// ---------------------------------------------------------------------------
// Validation of hand-authored semantic facts
// ---------------------------------------------------------------------------

for (const keyword of KEYWORDS) {
  requireLiteral(keyword);
}

for (const keyword of Object.keys(KEYWORD_NODE_COVERAGE)) {
  if (!(KEYWORDS as readonly string[]).includes(keyword)) {
    fail(`KEYWORD_NODE_COVERAGE has a keyword outside KEYWORDS: ${keyword}`);
  }
  for (const node of KEYWORD_NODE_COVERAGE[keyword]) {
    requireNode(node);
  }
}

for (const value of [...RESERVED_TYPES, ...PRELUDE_BOUNDS, ...OPERATORS]) {
  requireLiteral(value);
}

for (const node of [...OUTLINE_NODES, ...FUNCTION_NODES, ...CLASS_NODES]) {
  requireNode(node);
}

const externalTokens = new Set(grammar.externals.map((external) => publicNode(external.name)));
for (const [open, close] of BRACKET_TOKENS.named) {
  if (!externalTokens.has(open) || !externalTokens.has(close)) {
    fail(`bracket tokens are not external tokens: ${open}/${close}`);
  }
}
for (const node of ['on_declaration', 'function_declaration', 'test_declaration', 'lambda_expression']) {
  if (!(FUNCTION_NODES as readonly string[]).includes(node)) {
    fail(`FUNCTION_NODES is missing ${node}`);
  }
}

// ---------------------------------------------------------------------------
// highlights.scm
// ---------------------------------------------------------------------------

const highlights: string[] = [];

highlights.push('(identifier) @variable');
highlights.push('');
highlights.push(
  literalGroup(
    KEYWORDS.filter((keyword) => !(keyword in KEYWORD_NODE_COVERAGE)),
    'keyword',
  ),
);
highlights.push('');
highlights.push('(visibility) @keyword');
highlights.push('(break_statement) @keyword');
highlights.push('(continue_statement) @keyword');
highlights.push('(self_expression) @variable.special');
highlights.push('(self_parameter "self" @variable.special)');
highlights.push('');
highlights.push('(comment) @comment');
highlights.push('(doc_comment) @comment.doc');
highlights.push('');
highlights.push('(integer_literal) @number');
highlights.push('(float_literal) @number');
highlights.push('(boolean_literal) @boolean');
highlights.push('(string) @string');
highlights.push('(string_content) @string');
highlights.push('(escape_sequence) @string.escape');
highlights.push('(interpolation) @embedded');
highlights.push('');

highlights.push(literalGroup(RESERVED_TYPES, 'type.builtin'));
highlights.push('(named_type name: (type_identifier) @type)');
highlights.push('(type_identifier) @type');
highlights.push(
  `((type_identifier) @type.builtin\n  (#any-of? @type.builtin ${PRELUDE_TYPES.map(quote).join(' ')}))`,
);
highlights.push('(type_parameter name: (type_identifier) @type)');
highlights.push('(bound) @type.builtin');
highlights.push('(constant) @constant');
highlights.push('');

highlights.push('(function_declaration name: (identifier) @function)');
highlights.push('(on_declaration name: (identifier) @function)');
highlights.push('(struct_declaration name: (type_identifier) @type)');
highlights.push('(opaque_struct_declaration name: (type_identifier) @type)');
highlights.push('(enum_declaration name: (type_identifier) @type)');
highlights.push('(impl_declaration type: (type_identifier) @type)');
highlights.push('(variant name: (type_identifier) @variant)');
highlights.push('(field_declaration name: (identifier) @property)');
highlights.push('(field_initializer name: (identifier) @property)');
highlights.push('(field_pattern name: (identifier) @property)');
highlights.push('(parameter name: (identifier) @variable.parameter)');
highlights.push('(const_declaration name: (constant) @constant)');
highlights.push(
  `((on_declaration name: (identifier) @keyword)\n  (#any-of? @keyword ${CONTEXTUAL_ON_NAMES.map(quote).join(' ')}))`,
);
highlights.push('');

highlights.push('(field_expression field: (identifier) @property)');
highlights.push('(call_expression function: (identifier) @function)');
highlights.push('(call_expression function: (field_expression field: (identifier) @function))');
highlights.push('(call_expression function: (type_identifier) @constructor)');
highlights.push(
  `((call_expression function: (identifier) @function)\n  (#any-of? @function ${PRELUDE_FUNCTIONS.map(quote).join(' ')}))`,
);
highlights.push('(wildcard_pattern) @variable.special');
highlights.push('(binding_pattern name: (identifier) @variable)');
highlights.push('(variant_pattern name: (type_identifier) @variant)');
highlights.push('(variant_pattern qualifier: (type_identifier) @type)');
highlights.push('(struct_pattern type: (type_identifier) @type)');
highlights.push('(struct_literal name: (type_identifier) @type)');
highlights.push(
  `((type_identifier) @constructor\n  (#any-of? @constructor ${PRELUDE_VARIANTS.map(quote).join(' ')}))`,
);
highlights.push(
  `((variant_pattern name: (type_identifier) @constructor)\n  (#any-of? @constructor ${PRELUDE_VARIANTS.map(quote).join(' ')}))`,
);
highlights.push('');
highlights.push(literalGroup(OPERATORS, 'operator'));
highlights.push('');
highlights.push(literalGroup(['(', ')', '[', ']'], 'punctuation.bracket'));
highlights.push(
  [...new Set(BRACKET_TOKENS.named.flat())].map((token) => `(${token}) @punctuation.bracket`).join('\n'),
);
highlights.push(literalGroup([',', ':', '.'], 'punctuation.delimiter'));

// ---------------------------------------------------------------------------
// brackets.scm
// ---------------------------------------------------------------------------

const brackets: string[] = [];
for (const [open, close] of BRACKET_TOKENS.anonymous) {
  brackets.push(`((${quote(open)} @open ${quote(close)} @close))`);
}
for (const [open, close] of BRACKET_TOKENS.named) {
  brackets.push(`(((${open}) @open (${close}) @close))`);
}

// ---------------------------------------------------------------------------
// indents.scm
// ---------------------------------------------------------------------------

const indents = [
  '(_ (block_lbrace) @start (rbrace) @end) @indent',
  '(_ (list_lbrace) @start (rbrace) @end) @indent',
  '(_ (map_lbrace) @start (rbrace) @end) @indent',
  '(_ "(" @start ")" @end) @indent',
  '(_ "[" @start "]" @end) @indent',
  '(match_arm) @indent',
  '(if_expression) @start.if',
];

// ---------------------------------------------------------------------------
// outline.scm
// ---------------------------------------------------------------------------

const outline: string[] = [];
for (const nodeName of OUTLINE_NODES) {
  const fieldName = OUTLINE_NAME_FIELDS[nodeName] ?? 'name';
  const field = requireField(nodeName, fieldName);
  const childType = field.types[0]?.type;
  if (!childType) {
    fail(`outline field ${nodeName}.${fieldName} has no type`);
  }

  if (childType === 'string') {
    outline.push(`(${nodeName}\n  ${fieldName}: (string (string_content) @name)) @item`);
  } else {
    outline.push(`(${nodeName}\n  ${fieldName}: (${childType}) @name) @item`);
  }
}

// ---------------------------------------------------------------------------
// textobjects.scm
// ---------------------------------------------------------------------------

const textobjects: string[] = [];
for (const nodeName of FUNCTION_NODES) {
  const field = requireField(nodeName, 'body');
  if (!field.types.some((type) => type.type === 'block')) {
    fail(`function text object node has no block body: ${nodeName}`);
  }
  if (!field.required) {
    textobjects.push(`(${nodeName} !body) @function.around`);
  }
  textobjects.push(`(${nodeName}\n  body: (block) @function.inside) @function.around`);
}
textobjects.push('(comment)+ @comment.around');
textobjects.push('(doc_comment)+ @comment.around');

for (const nodeName of CLASS_NODES) {
  const node = requireNode(nodeName);
  const bodyField = node.fields?.body;
  if (bodyField) {
    const bodyType = bodyField.types[0]?.type;
    if (!bodyType) {
      fail(`class body field has no type: ${nodeName}.body`);
    }
    textobjects.push(`(${nodeName}\n  body: (${bodyType}) @class.inside) @class.around`);
  } else {
    textobjects.push(`(${nodeName}) @class.around`);
  }
}

// ---------------------------------------------------------------------------
// runnables.scm / overrides.scm / locals.scm / tags.scm
// ---------------------------------------------------------------------------

const runnables = RUNNABLES.map((runnable) => runnable.query).join('\n\n');
const overrides = ['(string) @string', '(comment) @comment.inclusive', '(doc_comment) @comment.inclusive'];

const locals: string[] = [];
for (const nodeName of [
  'function_declaration',
  'on_declaration',
  'lambda_expression',
  'block',
  'test_declaration',
  'for_statement',
  'match_arm',
]) {
  requireNode(nodeName);
  locals.push(`(${nodeName}) @local.scope`);
}
locals.push('');
for (const nodeName of ['parameter', 'self_parameter', 'binding_pattern', 'field_pattern']) {
  requireNode(nodeName);
}
locals.push('(parameter name: (identifier) @local.definition)');
locals.push('(self_parameter) @local.definition');
locals.push('(binding_pattern name: (identifier) @local.definition)');
locals.push('(field_pattern name: (identifier) @local.definition)');
locals.push('');
locals.push('(identifier) @local.reference');

const tags = [
  '(function_declaration name: (identifier) @name) @definition.function',
  '(on_declaration name: (identifier) @name) @definition.method',
  '(struct_declaration name: (type_identifier) @name) @definition.class',
  '(opaque_struct_declaration name: (type_identifier) @name) @definition.class',
  '(enum_declaration name: (type_identifier) @name) @definition.enum',
  '(variant name: (type_identifier) @name) @definition.enum_member',
  '(const_declaration name: (constant) @name) @definition.constant',
  '(impl_declaration type: (type_identifier) @name) @definition.interface',
  '',
  '(identifier) @reference',
  '(type_identifier) @reference',
];

// ---------------------------------------------------------------------------
// config.toml and tasks.json
// ---------------------------------------------------------------------------

const config = `# Generated by scripts/generate-queries.ts. Do not edit.
name = "Martite"
grammar = "martite"
path_suffixes = ["mt", "mti"]
line_comments = ["// "]
tab_size = 4
hard_tabs = false
autoclose_before = ";:.,=}])"
word_characters = ["_"]

brackets = [
  { start = "{", end = "}", close = true, newline = true, not_in = ["comment"] },
  { start = "[", end = "]", close = true, newline = true, not_in = ["comment"] },
  { start = "(", end = ")", close = true, newline = true, not_in = ["comment"] },
  { start = "\\"", end = "\\"", close = true, newline = false, not_in = ["comment", "string"] },
]

decrease_indent_patterns = [
  { pattern = "^\\\\s*else\\\\b", valid_after = ["if"] },
]
`;

const tasks = `${JSON.stringify(
  TASKS.map((task) => ({
    label: task.label,
    command: task.command,
    args: task.args,
    save: 'all',
    ...(task.tags ? { tags: task.tags } : {}),
  })),
  null,
  2,
)}\n`;

// ---------------------------------------------------------------------------
// Coverage and capture validation
// ---------------------------------------------------------------------------

const generatedQueryText = [
  ...highlights,
  ...brackets,
  ...indents,
  ...outline,
  ...textobjects,
  runnables,
  ...overrides,
  ...locals,
  ...tags,
].join('\n');

const uncovered: string[] = [];
for (const node of nodeMap.keys()) {
  if (node.startsWith('_')) {
    continue;
  }
  if (generatedQueryText.includes(node) || IGNORED_NODES.has(node)) {
    continue;
  }
  uncovered.push(node);
}
if (uncovered.length > 0) {
  fail(`named nodes are not covered by any query and are not ignored:\n  ${uncovered.join('\n  ')}`);
}

for (const ignored of IGNORED_NODES) {
  if (!nodeMap.has(ignored)) {
    fail(`IGNORED_NODES contains an unknown node: ${ignored}`);
  }
}

const capturePattern = /@([A-Za-z_][A-Za-z0-9_.]*)/g;
function validateCaptures(name: string, text: string, allowed: Set<string>): void {
  for (const match of text.matchAll(capturePattern)) {
    const capture = match[1].replace(/^_/, '').replace(/\.inclusive$/, '');
    if (!allowed.has(capture)) {
      fail(`${name} uses an unsupported capture: @${match[1]}`);
    }
  }
}

validateCaptures('highlights.scm', highlights.join('\n'), ZED_HIGHLIGHT_CAPTURES);
validateCaptures(
  'brackets.scm',
  brackets.join('\n'),
  new Set(['open', 'close']),
);
validateCaptures(
  'indents.scm',
  indents.join('\n'),
  new Set(['indent', 'start', 'end', 'outdent', 'start.if']),
);
validateCaptures('outline.scm', outline.join('\n'), new Set(['item', 'name']));
validateCaptures(
  'textobjects.scm',
  textobjects.join('\n'),
  new Set([
    'function.around',
    'function.inside',
    'class.around',
    'class.inside',
    'comment.around',
    'comment.inside',
  ]),
);
validateCaptures('runnables.scm', runnables, new Set(['run']));
validateCaptures(
  'overrides.scm',
  overrides.join('\n'),
  new Set(['string', 'comment']),
);
validateCaptures('locals.scm', locals.join('\n'), new Set(['local.scope', 'local.definition', 'local.reference']));
validateCaptures(
  'tags.scm',
  tags.join('\n'),
  new Set([
    'name',
    'definition.function',
    'definition.method',
    'definition.class',
    'definition.enum',
    'definition.enum_member',
    'definition.constant',
    'definition.interface',
    'reference',
  ]),
);

for (const task of TASKS) {
  for (const tag of task.tags ?? []) {
    if (!RUNNABLES.some((runnable) => runnable.tag === tag)) {
      fail(`task ${task.label} references unknown runnable tag: ${tag}`);
    }
  }
}
for (const runnable of RUNNABLES) {
  if (!TASKS.some((task) => task.tags?.includes(runnable.tag))) {
    fail(`runnable tag has no matching task: ${runnable.tag}`);
  }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

const header = (name: string) => `; Generated by scripts/generate-queries.ts for ${name}. Do not edit.\n\n`;

const grammarFiles = new Map<string, string>([
  [join(grammarDir, 'queries', 'highlights.scm'), header('tree-sitter highlights') + highlights.join('\n') + '\n'],
  [join(grammarDir, 'queries', 'locals.scm'), header('tree-sitter locals') + locals.join('\n') + '\n'],
  [join(grammarDir, 'queries', 'tags.scm'), header('tree-sitter tags') + tags.join('\n') + '\n'],
]);

const zedDir = resolve(grammarDir, '..', 'zed-martite');
const zedLanguageDir = join(zedDir, 'languages', 'martite');
const zedFiles = new Map<string, string>([
  [join(zedLanguageDir, 'highlights.scm'), header('Zed highlights') + highlights.join('\n') + '\n'],
  [join(zedLanguageDir, 'brackets.scm'), header('Zed brackets') + brackets.join('\n') + '\n'],
  [join(zedLanguageDir, 'indents.scm'), header('Zed indents') + indents.join('\n') + '\n'],
  [join(zedLanguageDir, 'outline.scm'), header('Zed outline') + outline.join('\n') + '\n'],
  [join(zedLanguageDir, 'textobjects.scm'), header('Zed text objects') + textobjects.join('\n') + '\n'],
  [join(zedLanguageDir, 'runnables.scm'), header('Zed runnables') + runnables + '\n'],
  [join(zedLanguageDir, 'overrides.scm'), header('Zed overrides') + overrides.join('\n') + '\n'],
  [join(zedLanguageDir, 'config.toml'), config],
  [join(zedLanguageDir, 'tasks.json'), tasks],
]);

export function generateQueries(check: boolean): boolean {
  const files = new Map(grammarFiles);
  if (existsSync(zedDir)) {
    for (const [path, content] of zedFiles) {
      files.set(path, content);
    }
  }

  const stale: string[] = [];
  for (const [path, expected] of files) {
    const normalized = expected.replaceAll('\r\n', '\n');
    if (!existsSync(path) || readFileSync(path, 'utf8').replaceAll('\r\n', '\n') !== normalized) {
      stale.push(path);
      if (!check) {
        mkdirSync(dirname(path), { recursive: true });
        writeFileSync(path, normalized);
      }
    }
  }

  if (check && stale.length > 0) {
    console.error('Generated queries are stale. Run `bun run generate` and commit them.');
    for (const path of stale) {
      console.error(`  ${path}`);
    }
    return false;
  }

  return true;
}

if (import.meta.main) {
  const check = process.argv.includes('--check');
  if (!generateQueries(check)) {
    process.exit(1);
  }
}
