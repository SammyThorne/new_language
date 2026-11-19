; Highlighting for Weby. Later patterns take priority over earlier ones (Zed and Neovim), so the
; general rules come first.

(identifier) @variable

(self) @variable.special

((identifier) @constant
  (#match? @constant "^_*[A-Z][A-Z0-9_]+$"))

; ---- types -----------------------------------------------------------------------------

(named_type (identifier) @type)

((named_type (identifier) @type.builtin)
  (#any-of? @type.builtin
    "int" "float" "bool" "str" "bytes" "list" "dict" "tuple" "frozendict" "Option" "Result"
    "i8" "i16" "i32" "i64" "u8" "u16" "u32" "u64" "f32" "f64"
    "Channel" "Mutex" "Atomic" "Task" "Error" "Self"))

(type_parameter name: (identifier) @type)
(struct_definition name: (identifier) @type)
(enum_definition name: (identifier) @type)
(trait_definition name: (identifier) @type)
(type_alias name: (identifier) @type)

(variant name: (identifier) @variant)

; In a pattern, a capitalised name or the last part of a dotted one is a variant; a lower-case
; bare name binds a value.
(variant_pattern name: (dotted_name (identifier) @type))
(variant_pattern name: (dotted_name (identifier) @variant .))
((variant_pattern name: (dotted_name . (identifier) @variable .))
  (#match? @variable "^[a-z_]"))

; ---- functions ---------------------------------------------------------------------------

(function_definition name: (identifier) @function)

(call function: (identifier) @function)
(call function: (attribute attribute: (identifier) @function.method))
(call function: (subscript value: (identifier) @function))
(call function: (subscript value: (attribute attribute: (identifier) @function.method)))

((call function: (identifier) @function.builtin)
  (#any-of? @function.builtin
    "print" "len" "range" "str" "int" "float" "bool" "min" "max" "sorted" "sum" "abs" "panic"
    "enumerate" "zip" "any" "all" "iter" "next"))

((call function: (identifier) @constructor)
  (#match? @constructor "^[A-Z]"))
((call function: (attribute attribute: (identifier) @constructor))
  (#match? @constructor "^[A-Z]"))
((call function: (subscript value: (identifier) @constructor))
  (#match? @constructor "^[A-Z]"))

(annotation "@" @attribute name: (dotted_name (identifier) @attribute))

; ---- parameters, fields and arguments ---------------------------------------------------

(typed_parameter name: (identifier) @variable.parameter)
(default_parameter name: (identifier) @variable.parameter)
(lambda_parameters (identifier) @variable.parameter)

(attribute attribute: (identifier) @property)
(field_declaration name: (identifier) @property)
(variant_field name: (identifier) @property)
(keyword_argument name: (identifier) @property)
(keyword_pattern name: (identifier) @property)

(const_declaration name: (identifier) @constant)

; ---- literals ---------------------------------------------------------------------------

[(integer) (float)] @number
[(true) (false)] @boolean
(none) @constant.builtin

(string) @string
(docstring) @string.doc
(source_file . (expression_statement (string) @string.doc))
(function_definition body: (block . (expression_statement (string) @string.doc)))
(trait_definition body: (block . (expression_statement (string) @string.doc)))
(escape_sequence) @string.escape
(format_spec) @string.special

(interpolation "{" @punctuation.special "}" @punctuation.special)

(comment) @comment

(wildcard_pattern) @variable.special

; ---- keywords, operators and punctuation ------------------------------------------------

[
  "import" "from" "as"
  "def" "struct" "enum" "trait" "impl" "const" "type" "lambda"
  "pub" "priv"
  "if" "elif" "else" "while" "for" "in" "match" "case" "with"
  "return" "pass" "break" "continue" "assert" "spawn"
  "and" "or" "not" "is" "not in" "is not"
] @keyword

[
  "+" "-" "*" "/" "//" "%" "**" "~" "&" "|" "^" "<<" ">>"
  "<" "<=" "==" "!=" ">=" ">"
  "=" "+=" "-=" "*=" "/=" "//=" "%="
  "->" "?"
] @operator

["(" ")" "[" "]" "{" "}"] @punctuation.bracket
["," "." ":"] @punctuation.delimiter
(ellipsis) @punctuation.special
