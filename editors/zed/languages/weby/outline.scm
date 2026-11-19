(decorated_definition (annotation) @annotation)

(function_definition
  (visibility)? @context
  "def" @context
  name: (identifier) @name) @item

(struct_definition
  (visibility)? @context
  "struct" @context
  name: (identifier) @name) @item

(field_declaration
  name: (identifier) @name) @item

(enum_definition
  (visibility)? @context
  "enum" @context
  name: (identifier) @name) @item

(variant
  name: (identifier) @name) @item

(trait_definition
  (visibility)? @context
  "trait" @context
  name: (identifier) @name) @item

(impl_block
  "impl" @context
  trait: (_)? @name
  "for"? @context
  type: (_) @name) @item

(const_declaration
  (visibility)? @context
  "const" @context
  name: (identifier) @name) @item

(type_alias
  "type" @context
  name: (identifier) @name) @item
