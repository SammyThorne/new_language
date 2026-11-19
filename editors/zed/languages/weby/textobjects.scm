(function_definition
  body: (_) @function.inside) @function.around

(struct_definition
  body: (_) @class.inside) @class.around

(enum_definition
  body: (_) @class.inside) @class.around

(trait_definition
  body: (_) @class.inside) @class.around

(impl_block
  body: (_) @class.inside) @class.around

(comment)+ @comment.around
