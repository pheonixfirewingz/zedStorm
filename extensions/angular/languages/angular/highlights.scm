; Generic identifiers first: later patterns take precedence in Zed.
(identifier) @variable

; HTML structure

(tag_name) @tag

; Custom elements and components (<app-header>, <mat-card>) read as types, as in WebStorm.
((tag_name) @type
  (#match? @type "^[a-z][a-z0-9]*(-[a-z0-9]+)+$"))

; Angular's own structural elements.
((tag_name) @tag.builtin
  (#match? @tag.builtin "^(ng-container|ng-template|ng-content|router-outlet)$"))

(erroneous_end_tag_name) @tag

(doctype) @tag.doctype

(attribute_name) @attribute

[
  (attribute_value)
  (quoted_attribute_value)
] @string

(comment) @comment

(entity) @string.special

(text) @text

[
  "<"
  ">"
  "<!"
  "</"
  "/>"
] @punctuation.bracket

"=" @punctuation.delimiter

; Expressions

(call_expression
  function: (identifier) @function)

(member_expression
  property: (identifier) @property)

(member_expression
  call: (call_expression
    function: (identifier) @function.method))

(pair
  key: (identifier) @property)

(pipe_sequence
  (pipe_operator) @operator)

(pipe_call
  name: (identifier) @function.special)

(string) @string

(template_string) @string

(template_chars) @string

(regular_expression) @string.regex

(number) @number

[
  (unit)
  (style_unit)
] @type

[
  (unary_operator)
  (ternary_operator)
  (conditional_operator)
  (coalescing_operator)
] @operator

(binary_expression
  operator: _ @operator)

(template_substitution
  [
    "${"
    "}"
  ] @punctuation.special)

[
  ","
  ";"
  ":"
  "."
  "?."
] @punctuation.delimiter

; Bindings: [property], (event), [(twoWay)], [@animation], *structural

(property_binding
  [
    "["
    "]"
  ] @punctuation.bracket)

(property_binding
  (binding_name
    (identifier) @property))

(property_binding
  (binding_name
    (member_expression
      property: (identifier) @property)))

(class_binding
  (identifier) @keyword)

(class_binding
  (class_name) @property)

(event_binding
  [
    "("
    ")"
  ] @punctuation.bracket)

(event_binding
  (binding_name
    (identifier) @function))

(event_binding
  (binding_name
    (member_expression
      property: (identifier) @function)))

(two_way_binding
  [
    "[("
    ")]"
  ] @punctuation.bracket)

(two_way_binding
  (binding_name
    (identifier) @property))

(animation_binding
  [
    "[@"
    "]"
  ] @punctuation.bracket)

(animation_binding
  (binding_name
    (identifier) @function.special))

(structural_directive
  "*" @keyword.directive
  (identifier) @keyword.directive)

; Interpolation {{ ... }}

(interpolation
  [
    "{{"
    "}}"
  ] @punctuation.special)

; Control flow: @if, @else, @for, @empty, @switch, @case, @default, @defer, @placeholder,
; @loading, @error, @let

(control_keyword) @keyword.control

(if_statement
  "@" @keyword.control)

(else_if_statement
  "@" @keyword.control)

(else_statement
  "@" @keyword.control)

(for_statement
  "@" @keyword.control)

(empty_statement
  "@" @keyword.control)

(switch_statement
  "@" @keyword.control)

(case_statement
  "@" @keyword.control)

(default_statement
  "@" @keyword.control)

(defer_statement
  "@" @keyword.control)

(placeholder_statement
  "@" @keyword.control)

(loading_statement
  "@" @keyword.control)

(error_statement
  "@" @keyword.control)

(let_statement
  "@" @keyword.control)

; `of`, `track`, `as`, `on`, `when`, `after`, `minimum`, `let` inside control flow and directives.
(special_keyword) @keyword

(prefetch_keyword) @keyword

(icu_clause) @keyword

(icu_category) @constant

(for_declaration
  name: (identifier) @variable)

(if_reference
  (identifier) @variable)

(let_statement
  (assignment_expression
    name: (identifier) @variable))

(statement_block
  [
    "{"
    "}"
  ] @punctuation.bracket)

(switch_body
  [
    "{"
    "}"
  ] @punctuation.bracket)

; Embedded script and style
(script_element
  (raw_text) @embedded)

(style_element
  (raw_text) @embedded)
