(function_declaration name: (identifier) @name) @definition.function
(on_declaration name: (identifier) @name) @definition.method
(struct_declaration name: (type_identifier) @name) @definition.class
(opaque_struct_declaration name: (type_identifier) @name) @definition.class
(enum_declaration name: (type_identifier) @name) @definition.enum
(variant name: (type_identifier) @name) @definition.enum_member
(const_declaration name: (constant) @name) @definition.constant
(impl_declaration type: (type_identifier) @name) @definition.interface

(identifier) @reference
(type_identifier) @reference
