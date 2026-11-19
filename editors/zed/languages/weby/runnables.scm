; A run button beside `def main()` and beside each `@test` function.

((function_definition
  name: (identifier) @run @_name)
  (#eq? @_name "main")
  (#set! tag weby-main))

((decorated_definition
  (annotation
    name: (dotted_name (identifier) @_annotation))
  definition: (function_definition
    name: (identifier) @run))
  (#eq? @_annotation "test")
  (#set! tag weby-test))
