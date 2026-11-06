(comment) @annotation

(element
  (start_tag
    (tag_name) @name)) @item

(element
  (self_closing_tag
    (tag_name) @name)) @item

(if_statement
  "@" @context
  (control_keyword) @name
  condition: (if_condition) @context.extra) @item

(else_if_statement
  "@" @context
  (control_keyword) @name) @item

(else_statement
  "@" @context
  (control_keyword) @name) @item

(for_statement
  "@" @context
  (control_keyword) @name
  declaration: (for_declaration) @context.extra) @item

(empty_statement
  "@" @context
  (control_keyword) @name) @item

(switch_statement
  "@" @context
  (control_keyword) @name) @item

(case_statement
  "@" @context
  (control_keyword) @name) @item

(default_statement
  "@" @context
  (control_keyword) @name) @item

(defer_statement
  "@" @context
  (control_keyword) @name) @item

(placeholder_statement
  "@" @context
  (control_keyword) @name) @item

(loading_statement
  "@" @context
  (control_keyword) @name) @item

(error_statement
  "@" @context
  (control_keyword) @name) @item

(let_statement
  "@" @context
  (control_keyword) @context
  (assignment_expression
    name: (identifier) @name)) @item
