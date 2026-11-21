/**
 * A Tree-sitter grammar for Viper, for editors (Zed, Neovim, Helix) and GitHub. It follows the
 * grammar sketch in docs/01-language-design.md and the compiler's parser, but accepts a little
 * more than the compiler does (top-level statements, comparison chains), because an editor should
 * keep highlighting a file that is still wrong. Indentation is handled by the external scanner in
 * src/scanner.c, which works the way the compiler's lexer does.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const PREC = {
  lambda: -2,
  conditional: -1,
  parenthesized: 1,
  or: 10,
  and: 11,
  not: 12,
  compare: 13,
  bitwise_or: 14,
  bitwise_xor: 15,
  bitwise_and: 16,
  shift: 17,
  plus: 18,
  times: 19,
  unary: 20,
  power: 21,
  spawn: 22,
  call: 23,
};

const commaSep1 = (rule) => seq(rule, repeat(seq(',', rule)));
const commaSep = (rule) => optional(commaSep1(rule));
const trailingCommaSep = (rule) => optional(seq(commaSep1(rule), optional(',')));

/** The pieces of one kind of string literal: its opening and closing quote, and whether it is an
 * f-string (whose braces are interpolations). */
function stringRule($, open, close, multiline, interpolated) {
  const excluded = (close[0] === '"' ? '"' : "'") + '\\\\' + (interpolated ? '{}' : '') + (multiline ? '' : '\\n');
  const pieces = [
    alias(token.immediate(prec(1, new RegExp(`[^${excluded}]+`))), $.string_content),
    $.escape_sequence,
  ];
  if (multiline) {
    // A lone quote inside a triple-quoted string; three of them close it (longest match).
    pieces.push(alias(token.immediate(close[0]), $.string_content));
  }
  if (interpolated) {
    pieces.push($.interpolation, alias(token.immediate(choice('{{', '}}')), $.escape_sequence));
  }
  return seq(open, repeat(choice(...pieces)), token.immediate(close));
}

module.exports = grammar({
  name: 'viper',

  externals: ($) => [$._newline, $._indent, $._dedent],

  extras: ($) => [$.comment, /[\s\f﻿⁠​]|\r?\n/],

  word: ($) => $.identifier,

  supertypes: ($) => [$.expression, $.primary_expression, $.pattern, $.type],

  rules: {
    source_file: ($) => repeat($._statement),

    _statement: ($) => choice($._simple_statement_line, $._compound_statement),

    _simple_statement_line: ($) => seq($._simple_statement, $._newline),

    _simple_statement: ($) =>
      choice(
        $.import_statement,
        $.import_from_statement,
        $.const_declaration,
        $.type_alias,
        $.expression_statement,
        $.assignment,
        $.augmented_assignment,
        $.annotated_assignment,
        $.return_statement,
        $.pass_statement,
        $.break_statement,
        $.continue_statement,
        $.assert_statement,
      ),

    _compound_statement: ($) =>
      choice(
        $.function_definition,
        $.struct_definition,
        $.enum_definition,
        $.trait_definition,
        $.impl_block,
        $.decorated_definition,
        $.if_statement,
        $.while_statement,
        $.for_statement,
        $.match_statement,
        $.with_statement,
      ),

    // ---- imports and declarations ----------------------------------------------------

    dotted_name: ($) => prec(1, seq($.identifier, repeat(seq('.', $.identifier)))),

    import_statement: ($) => seq('import', field('module', $.dotted_name), optional(seq('as', field('alias', $.identifier)))),

    import_from_statement: ($) =>
      seq('from', field('module', $.dotted_name), 'import', commaSep1(choice($.identifier, $.aliased_import))),

    aliased_import: ($) => seq(field('name', $.identifier), 'as', field('alias', $.identifier)),

    visibility: (_) => choice('pub', 'priv'),

    const_declaration: ($) =>
      seq(
        optional($.visibility),
        'const',
        field('name', $.identifier),
        optional(seq(':', field('type', $.type))),
        '=',
        field('value', $.expression),
      ),

    type_alias: ($) => seq(optional($.visibility), 'type', field('name', $.identifier), '=', field('value', $.type)),

    annotation: ($) => seq('@', field('name', $.dotted_name), optional(field('arguments', $.argument_list)), $._newline),

    decorated_definition: ($) =>
      seq(
        repeat1($.annotation),
        field(
          'definition',
          choice($.function_definition, $.struct_definition, $.enum_definition, $.trait_definition),
        ),
      ),

    function_definition: ($) =>
      seq(
        optional($.visibility),
        'def',
        field('name', $.identifier),
        optional(field('type_parameters', $.type_parameters)),
        field('parameters', $.parameters),
        optional(seq('->', field('return_type', $.type))),
        choice(seq(':', field('body', $._suite)), $._newline),
      ),

    parameters: ($) => seq('(', trailingCommaSep($._parameter), ')'),

    _parameter: ($) => choice($.self, $.typed_parameter, $.default_parameter),

    typed_parameter: ($) => seq(field('name', $.identifier), ':', field('type', $.type)),

    default_parameter: ($) =>
      seq(field('name', $.identifier), ':', field('type', $.type), '=', field('value', $.expression)),

    type_parameters: ($) => seq('[', commaSep1($.type_parameter), optional(','), ']'),

    type_parameter: ($) => seq(field('name', $.identifier), optional(seq(':', field('bound', $.trait_bound)))),

    trait_bound: ($) => seq($.type, repeat(seq('+', $.type))),

    struct_definition: ($) =>
      seq(
        optional($.visibility),
        'struct',
        field('name', $.identifier),
        optional(field('type_parameters', $.type_parameters)),
        ':',
        field('body', $.struct_body),
      ),

    struct_body: ($) => seq($._indent, repeat1(choice($.field_declaration, $._docstring_line, $._pass_line)), $._dedent),

    field_declaration: ($) =>
      seq(
        repeat($.annotation),
        optional($.visibility),
        field('name', $.identifier),
        ':',
        field('type', $.type),
        optional(seq('=', field('default', $.expression))),
        $._newline,
      ),

    _docstring_line: ($) => seq(alias($.string, $.docstring), $._newline),

    _pass_line: ($) => seq($.pass_statement, $._newline),

    enum_definition: ($) =>
      seq(
        optional($.visibility),
        'enum',
        field('name', $.identifier),
        optional(field('type_parameters', $.type_parameters)),
        ':',
        field('body', $.enum_body),
      ),

    enum_body: ($) => seq($._indent, repeat1(choice($.variant, $._docstring_line, $._pass_line)), $._dedent),

    variant: ($) =>
      seq(
        field('name', $.identifier),
        optional(seq('(', trailingCommaSep(choice($.variant_field, $.type)), ')')),
        $._newline,
      ),

    variant_field: ($) => seq(field('name', $.identifier), ':', field('type', $.type)),

    trait_definition: ($) =>
      seq(
        optional($.visibility),
        'trait',
        field('name', $.identifier),
        optional(field('type_parameters', $.type_parameters)),
        optional(seq('(', field('supertraits', commaSep1($.type)), ')')),
        ':',
        field('body', $._suite),
      ),

    impl_block: ($) =>
      seq(
        'impl',
        optional(field('type_parameters', $.type_parameters)),
        optional(seq(field('trait', $.type), 'for')),
        field('type', $.type),
        ':',
        field('body', $._suite),
      ),

    // ---- statements -----------------------------------------------------------------

    _suite: ($) => choice(alias($._simple_statement_line, $.block), seq($._indent, $.block)),

    block: ($) => seq(repeat($._statement), $._dedent),

    expression_statement: ($) => choice($.expression, $.expression_list),

    expression_list: ($) => prec.right(seq($.expression, repeat1(seq(',', $.expression)), optional(','))),

    assignment: ($) =>
      seq(field('left', choice($.expression, $.expression_list)), '=', field('right', choice($.expression, $.expression_list))),

    augmented_assignment: ($) =>
      seq(
        field('left', $.expression),
        field('operator', choice('+=', '-=', '*=', '/=', '//=', '%=')),
        field('right', $.expression),
      ),

    annotated_assignment: ($) =>
      seq(
        field('left', choice($.identifier, $.self, $.attribute)),
        ':',
        field('type', $.type),
        optional(seq('=', field('right', $.expression))),
      ),

    return_statement: ($) => seq('return', optional(choice($.expression, $.expression_list))),

    pass_statement: (_) => prec.left('pass'),

    break_statement: (_) => prec.left('break'),

    continue_statement: (_) => prec.left('continue'),

    assert_statement: ($) => seq('assert', field('condition', $.expression), optional(seq(',', field('message', $.expression)))),

    if_statement: ($) =>
      seq(
        'if',
        field('condition', $.expression),
        ':',
        field('consequence', $._suite),
        repeat(field('alternative', $.elif_clause)),
        optional(field('alternative', $.else_clause)),
      ),

    elif_clause: ($) => seq('elif', field('condition', $.expression), ':', field('consequence', $._suite)),

    else_clause: ($) => seq('else', ':', field('body', $._suite)),

    while_statement: ($) => seq('while', field('condition', $.expression), ':', field('body', $._suite)),

    for_statement: ($) =>
      seq('for', field('left', $._for_target), 'in', field('right', choice($.expression, $.expression_list)), ':', field('body', $._suite)),

    _for_target: ($) => choice($.identifier, $.pattern_list, $.tuple_pattern),

    pattern_list: ($) => seq(choice($.identifier, $.tuple_pattern), repeat1(seq(',', choice($.identifier, $.tuple_pattern)))),

    with_statement: ($) =>
      seq('with', field('value', $.expression), optional(seq('as', field('alias', $.identifier))), ':', field('body', $._suite)),

    match_statement: ($) =>
      seq('match', field('subject', $.expression), ':', $._indent, repeat1(field('case', $.case_clause)), $._dedent),

    case_clause: ($) =>
      seq('case', field('pattern', $.pattern), optional(seq('if', field('guard', $.expression))), ':', field('consequence', $._suite)),

    // ---- patterns -------------------------------------------------------------------

    pattern: ($) => choice($.or_pattern, $.wildcard_pattern, $.literal_pattern, $.variant_pattern, $.tuple_pattern),

    or_pattern: ($) => prec.left(seq($.pattern, '|', $.pattern)),

    wildcard_pattern: (_) => prec(2, '_'),

    literal_pattern: ($) => choice($.integer, $.float, $.string, $.true, $.false, seq('-', choice($.integer, $.float))),

    // A bare name is a variant without fields or a new binding; only the compiler can tell.
    variant_pattern: ($) =>
      prec(1, seq(field('name', choice($.dotted_name, $.none)), optional(seq('(', trailingCommaSep(choice($.pattern, $.keyword_pattern)), ')')))),

    keyword_pattern: ($) => seq(field('name', $.identifier), '=', field('value', $.pattern)),

    tuple_pattern: ($) => seq('(', trailingCommaSep($.pattern), ')'),

    // ---- types ----------------------------------------------------------------------

    type: ($) =>
      choice($.named_type, $.generic_type, $.optional_type, $.union_type, $.function_type, $.parenthesized_type, $.none),

    named_type: ($) => prec.right(seq($.identifier, repeat(seq('.', $.identifier)))),

    generic_type: ($) => seq($.named_type, $.type_arguments),

    type_arguments: ($) => seq('[', commaSep1(choice($.type, $.ellipsis)), optional(','), ']'),

    optional_type: ($) => prec(2, seq($.type, '?')),

    union_type: ($) => prec.left(1, seq($.type, '|', $.type)),

    function_type: ($) =>
      prec.right(seq('def', '(', trailingCommaSep($.type), ')', optional(seq('->', field('return_type', $.type))))),

    parenthesized_type: ($) => seq('(', $.type, ')'),

    ellipsis: (_) => '...',

    // ---- expressions ----------------------------------------------------------------

    expression: ($) =>
      choice(
        $.lambda,
        $.conditional_expression,
        $.boolean_operator,
        $.not_operator,
        $.comparison_operator,
        $.binary_operator,
        $.unary_operator,
        $.spawn_expression,
        $.primary_expression,
      ),

    primary_expression: ($) =>
      choice(
        $.identifier,
        $.self,
        $.integer,
        $.float,
        $.string,
        $.true,
        $.false,
        $.none,
        $.attribute,
        $.subscript,
        $.call,
        $.try_expression,
        $.list,
        $.dict,
        $.tuple,
        $.parenthesized_expression,
        $.list_comprehension,
        $.dict_comprehension,
      ),

    lambda: ($) =>
      prec(PREC.lambda, seq('lambda', optional(field('parameters', $.lambda_parameters)), ':', field('body', $.expression))),

    lambda_parameters: ($) => commaSep1($.identifier),

    conditional_expression: ($) =>
      prec.right(PREC.conditional, seq($.expression, 'if', field('condition', $.expression), 'else', $.expression)),

    boolean_operator: ($) =>
      choice(
        prec.left(PREC.and, seq(field('left', $.expression), field('operator', 'and'), field('right', $.expression))),
        prec.left(PREC.or, seq(field('left', $.expression), field('operator', 'or'), field('right', $.expression))),
      ),

    not_operator: ($) => prec(PREC.not, seq('not', field('argument', $.expression))),

    comparison_operator: ($) =>
      prec.left(
        PREC.compare,
        seq(
          field('left', $.expression),
          field(
            'operator',
            choice('<', '<=', '==', '!=', '>=', '>', 'in', alias(seq('not', 'in'), 'not in'), 'is', alias(seq('is', 'not'), 'is not')),
          ),
          field('right', $.expression),
        ),
      ),

    binary_operator: ($) => {
      const table = [
        [prec.left, '+', PREC.plus],
        [prec.left, '-', PREC.plus],
        [prec.left, '*', PREC.times],
        [prec.left, '/', PREC.times],
        [prec.left, '//', PREC.times],
        [prec.left, '%', PREC.times],
        [prec.right, '**', PREC.power],
        [prec.left, '|', PREC.bitwise_or],
        [prec.left, '^', PREC.bitwise_xor],
        [prec.left, '&', PREC.bitwise_and],
        [prec.left, '<<', PREC.shift],
        [prec.left, '>>', PREC.shift],
      ];
      return choice(
        ...table.map(([fn, operator, precedence]) =>
          // @ts-ignore
          fn(precedence, seq(field('left', $.expression), field('operator', operator), field('right', $.expression))),
        ),
      );
    },

    unary_operator: ($) => prec(PREC.unary, seq(field('operator', choice('-', '+', '~')), field('argument', $.expression))),

    spawn_expression: ($) => prec(PREC.spawn, seq('spawn', field('call', $.expression))),

    attribute: ($) => prec(PREC.call, seq(field('object', $.primary_expression), '.', field('attribute', $.identifier))),

    subscript: ($) =>
      prec(
        PREC.call,
        seq(field('value', $.primary_expression), '[', commaSep1(field('subscript', choice($.expression, $.slice))), optional(','), ']'),
      ),

    slice: ($) => seq(optional($.expression), ':', optional($.expression)),

    call: ($) => prec(PREC.call, seq(field('function', $.primary_expression), field('arguments', $.argument_list))),

    try_expression: ($) => prec(PREC.call, seq($.primary_expression, '?')),

    argument_list: ($) => seq('(', trailingCommaSep(choice($.expression, $.keyword_argument)), ')'),

    keyword_argument: ($) => seq(field('name', $.identifier), '=', field('value', $.expression)),

    list: ($) => seq('[', trailingCommaSep($.expression), ']'),

    dict: ($) => seq('{', trailingCommaSep($.pair), '}'),

    pair: ($) => seq(field('key', $.expression), ':', field('value', $.expression)),

    tuple: ($) => seq('(', optional(seq($.expression, ',', trailingCommaSep($.expression))), ')'),

    parenthesized_expression: ($) => prec(PREC.parenthesized, seq('(', $.expression, ')')),

    list_comprehension: ($) => seq('[', field('body', $.expression), $._comprehension_clauses, ']'),

    dict_comprehension: ($) => seq('{', field('body', $.pair), $._comprehension_clauses, '}'),

    _comprehension_clauses: ($) => seq($.for_in_clause, repeat(choice($.for_in_clause, $.if_clause))),

    for_in_clause: ($) =>
      prec.left(seq('for', field('left', $._for_target), 'in', field('right', $.expression))),

    if_clause: ($) => seq('if', $.expression),

    // ---- literals -------------------------------------------------------------------

    self: (_) => 'self',
    true: (_) => 'True',
    false: (_) => 'False',
    none: (_) => 'None',

    integer: (_) => token(choice(/0[xX][0-9a-fA-F_]+/, /0[bB][01_]+/, /0[oO][0-7_]+/, /[0-9][0-9_]*/)),

    float: (_) => token(choice(/[0-9][0-9_]*\.[0-9][0-9_]*([eE][+-]?[0-9_]+)?/, /[0-9][0-9_]*[eE][+-]?[0-9_]+/)),

    identifier: (_) => /[_\p{XID_Start}][_\p{XID_Continue}]*/,

    string: ($) =>
      choice(
        stringRule($, '"""', '"""', true, false),
        stringRule($, "'''", "'''", true, false),
        stringRule($, '"', '"', false, false),
        stringRule($, "'", "'", false, false),
        alias($._fstring, $.fstring),
      ),

    _fstring: ($) =>
      choice(
        stringRule($, 'f"""', '"""', true, true),
        stringRule($, "f'''", "'''", true, true),
        stringRule($, 'f"', '"', false, true),
        stringRule($, "f'", "'", false, true),
      ),

    interpolation: ($) =>
      seq('{', field('expression', $.expression), optional(field('format_spec', $.format_spec)), '}'),

    format_spec: (_) => seq(':', optional(token.immediate(prec(1, /[^{}\n]+/)))),

    escape_sequence: (_) =>
      token.immediate(prec(2, choice(/\\x[0-9a-fA-F]{2}/, /\\u\{[0-9a-fA-F]+\}/, /\\u[0-9a-fA-F]{4}/, /\\[^xu]/))),

    comment: (_) => token(seq('#', /.*/)),
  },
});
