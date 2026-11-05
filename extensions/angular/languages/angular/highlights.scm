; HTML tags and structure
(tag_name) @tag
(erroneous_end_tag_name) @keyword
(doctype) @constant
(entity) @constant
(comment) @comment
(attribute_name) @attribute
(attribute_value) @string
(quoted_attribute_value) @string

; Angular custom components (kebab-case tags like <app-header>)
((tag_name) @type
  (#match? @type "^[a-z0-9]+(-[a-z0-9]+)+$"))

; Angular structural directives (*ngIf, *ngFor, *ngSwitchCase, etc.)
(structural_directive
  (identifier) @property.special)

((attribute_name) @property.special
  (#match? @property.special "^\\*"))

; Angular property bindings: [property]="value"
(property_binding
  "[" @punctuation.bracket
  (binding_name) @property
  "]" @punctuation.bracket)

; Angular event bindings: (click)="handleClick()"
(event_binding
  "(" @punctuation.bracket
  (binding_name) @function.method
  ")" @punctuation.bracket)

; Angular two-way banana-in-a-box bindings: [(ngModel)]="value"
(two_way_binding
  "[(" @punctuation.bracket
  (binding_name) @property.special
  ")]" @punctuation.bracket)

; Template reference variables: #myRef or #myRef="exportAs"
(template_reference
  "#" @punctuation.special
  (identifier) @variable)

; Pipe handling: value | pipeName:arg1:arg2
(pipe_sequence
  (pipe_operator) @operator
  (pipe_call name: (identifier) @function))

; Angular interpolation: {{ expression }}
(interpolation
  "{{" @punctuation.special
  "}}" @punctuation.special)

; Angular 17+ Modern Control Flow blocks
(control_keyword) @keyword.control
(special_keyword) @keyword.special
(let_statement (control_keyword) @keyword.control)

; @if, @else if, @else
((control_keyword) @keyword.control
  (#match? @keyword.control "^@(if|else(\\s+if)?)$"))

; @for, @empty
((control_keyword) @keyword.control
  (#match? @keyword.control "^@(for|empty)$"))

; @switch, @case, @default
((control_keyword) @keyword.control
  (#match? @keyword.control "^@(switch|case|default)$"))

; @defer, @placeholder, @loading, @error
((control_keyword) @keyword.control
  (#match? @keyword.control "^@(defer|placeholder|loading|error)$"))

; @let statement
((control_keyword) @keyword.control
  (#match? @keyword.control "^@let$"))

; Expressions inside interpolations and bindings
(call_expression
  function: (identifier) @function)

(call_expression
  arguments: (arguments) @parameter)

(member_expression
  property: (identifier) @property)

(expression
  (identifier) @variable)

; Punctuation and brackets
[
  "<"
  ">"
  "<!""
  "</"
  "/>"
] @punctuation.bracket

"=" @operator
"@" @keyword.special
"*" @property.special
"|" @operator

; Script & Style embedded blocks
(script_element (raw_text) @embedded)
(style_element (raw_text) @embedded)
