(_ "[" "]" @end) @indent
(_ "{" "}" @end) @indent
(_ "(" ")" @end) @indent

; The statements that `decrease_indent_patterns` in config.toml can line up with.
(if_statement) @start.if
(elif_clause) @start.elif
(else_clause) @start.else
(match_statement) @start.match
(case_clause) @start.case
(for_statement) @start.for
(while_statement) @start.while
(with_statement) @start.with
(function_definition) @start.def
