use tree_sitter::Language;

/// Supported languages with their tree-sitter grammars and symbol queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Python,
    TypeScript,
    Tsx,
    JavaScript,
    Go,
    C,
    Cpp,
}

impl Lang {
    /// Detect language from file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Self::Rust),
            "py" | "pyi" => Some(Self::Python),
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" | "mjs" | "cjs" | "jsx" => Some(Self::JavaScript),
            "go" => Some(Self::Go),
            "c" | "h" => Some(Self::C),
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some(Self::Cpp),
            _ => None,
        }
    }

    /// Get the tree-sitter Language for this lang.
    pub fn grammar(&self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        }
    }

    /// Get the symbol extraction query for this language.
    /// Returns (query_string, capture_names_mapping).
    pub fn symbol_query(&self) -> &'static str {
        match self {
            Self::Rust => RUST_QUERY,
            Self::Python => PYTHON_QUERY,
            Self::TypeScript | Self::Tsx => TYPESCRIPT_QUERY,
            Self::JavaScript => JAVASCRIPT_QUERY,
            Self::Go => GO_QUERY,
            Self::C => C_QUERY,
            Self::Cpp => CPP_QUERY,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::JavaScript => "javascript",
            Self::Go => "go",
            Self::C => "c",
            Self::Cpp => "cpp",
        }
    }
}

// ---- Per-language queries ----
// Each query captures:
//   @name      - the symbol name
//   @kind      - used to tag the capture pattern (via pattern index)
//   @signature - the full signature (for functions)
//   @doc       - doc comments

const RUST_QUERY: &str = r#"
; Functions
(function_item
  name: (identifier) @name
) @function

; Structs
(struct_item
  name: (type_identifier) @name
) @struct

; Enums
(enum_item
  name: (type_identifier) @name
) @enum

; Traits
(trait_item
  name: (type_identifier) @name
) @trait

; Impl blocks
(impl_item
  type: (_) @name
) @impl

; Type aliases
(type_item
  name: (type_identifier) @name
) @type_alias

; Constants
(const_item
  name: (identifier) @name
) @const

; Static items
(static_item
  name: (identifier) @name
) @static

; Use declarations
(use_declaration
  argument: (_) @name
) @import

; Macros
(macro_definition
  name: (identifier) @name
) @macro
"#;

const PYTHON_QUERY: &str = r#"
; Functions
(function_definition
  name: (identifier) @name
) @function

; Classes
(class_definition
  name: (identifier) @name
) @class

; Imports
(import_statement
  name: (dotted_name) @name
) @import

(import_from_statement
  module_name: (dotted_name) @name
) @import

; Assignments at module level (constants)
(expression_statement
  (assignment
    left: (identifier) @name
  )
) @variable

; Decorated definitions
(decorated_definition
  definition: (function_definition
    name: (identifier) @name
  )
) @function

(decorated_definition
  definition: (class_definition
    name: (identifier) @name
  )
) @class
"#;

const TYPESCRIPT_QUERY: &str = r#"
; Functions
(function_declaration
  name: (identifier) @name
) @function

; Classes
(class_declaration
  name: (type_identifier) @name
) @class

; Interfaces
(interface_declaration
  name: (type_identifier) @name
) @interface

; Type aliases
(type_alias_declaration
  name: (type_identifier) @name
) @type_alias

; Enums
(enum_declaration
  name: (identifier) @name
) @enum

; Variable declarations (const/let/var at top level)
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
  )
) @variable

; Exports
(export_statement
  declaration: (function_declaration
    name: (identifier) @name
  )
) @function

(export_statement
  declaration: (class_declaration
    name: (type_identifier) @name
  )
) @class

; Imports
(import_statement
  source: (string) @name
) @import

; Method definitions inside classes
(method_definition
  name: (property_identifier) @name
) @function
"#;

const JAVASCRIPT_QUERY: &str = r#"
; Functions
(function_declaration
  name: (identifier) @name
) @function

; Classes
(class_declaration
  name: (identifier) @name
) @class

; Variable declarations
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
  )
) @variable

; Exports
(export_statement
  declaration: (function_declaration
    name: (identifier) @name
  )
) @function

(export_statement
  declaration: (class_declaration
    name: (identifier) @name
  )
) @class

; Imports
(import_statement
  source: (string) @name
) @import

; Method definitions
(method_definition
  name: (property_identifier) @name
) @function
"#;

const GO_QUERY: &str = r#"
; Functions
(function_declaration
  name: (identifier) @name
) @function

; Methods
(method_declaration
  name: (field_identifier) @name
) @function

; Type declarations (struct, interface, etc.)
(type_declaration
  (type_spec
    name: (type_identifier) @name
  )
) @type_alias

; Import declarations
(import_spec
  path: (interpreted_string_literal) @name
) @import

; Constants
(const_declaration
  (const_spec
    name: (identifier) @name
  )
) @const

; Variables
(var_declaration
  (var_spec
    name: (identifier) @name
  )
) @variable
"#;

const C_QUERY: &str = r#"
; Function definitions
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name
  )
) @function

; Function declarations (prototypes)
(declaration
  declarator: (function_declarator
    declarator: (identifier) @name
  )
) @function

; Struct definitions
(struct_specifier
  name: (type_identifier) @name
) @struct

; Enum definitions
(enum_specifier
  name: (type_identifier) @name
) @enum

; Type definitions
(type_definition
  declarator: (type_identifier) @name
) @type_alias

; Includes
(preproc_include
  path: (_) @name
) @import
"#;

const CPP_QUERY: &str = r#"
; Function definitions
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name
  )
) @function

; Function definitions with qualified names
(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier) @name
  )
) @function

; Class definitions
(class_specifier
  name: (type_identifier) @name
) @class

; Struct definitions
(struct_specifier
  name: (type_identifier) @name
) @struct

; Enum definitions
(enum_specifier
  name: (type_identifier) @name
) @enum

; Namespace definitions
(namespace_definition
  name: (namespace_identifier) @name
) @namespace

; Type definitions
(type_definition
  declarator: (type_identifier) @name
) @type_alias

; Includes
(preproc_include
  path: (_) @name
) @import
"#;
