# Supported Languages

## Currently Supported (9 languages)

### Python (.py, .pyi)
- Functions, classes, methods
- Imports (import, from...import)
- Function calls
- Inheritance (base classes)
- Raise statements
- Type annotation references (parameter types, return types)
- Decorator references (`@decorator`)
- Exception handler type references (`except ValueError:`)
- Async functions
- Docstrings
- Variable assignments (module-level and class-level)
- Visibility: public, _protected, __private, __dunder__

### TypeScript (.ts, .tsx)
- Functions (declaration and arrow), classes, methods, interfaces, enums, type aliases
- Imports (ES modules)
- Function calls, `new` expressions, throw statements
- Inheritance (extends), interface extends, implements
- Type annotation references (parameter types, return types, generic types)
- Async functions
- JSDoc comments
- Class fields with visibility (public/private/protected TS modifiers, #private, _convention)

### JavaScript (.js, .jsx, .mjs, .cjs)
- Functions (declaration and arrow), classes, methods
- Imports (ES modules)
- Function calls, `new` expressions, throw statements
- Inheritance (extends)
- Async functions
- JSDoc comments
- Class fields with visibility (#private, _convention)

### Rust (.rs)
- Functions, structs, enums, traits, type aliases, constants/statics
- Use declarations (use statements)
- Function calls, macro invocations (tracked as `name!`)
- Trait implementations (impl Trait for Type -> inherits edge)
- Type references in function signatures (parameter types, return types, generic types)
- Async functions
- Doc comments (///)
- Methods within impl blocks (linked to parent type)
- Visibility: pub (public), no modifier (private), pub(crate) (public)

### Go (.go)
- Functions, methods (with receiver type linkage)
- Structs, interfaces (as class symbols)
- Imports (single and grouped)
- Function and method calls (including selector expressions like `fmt.Println`)
- Interface embedding (inherits edges)
- Composite literal type references (`MyStruct{...}`)
- Type references in function signatures (parameter types, return types)
- Constants and variables (single and grouped)
- Doc comments (`//` preceding declarations)
- Visibility: Exported (uppercase) = public, unexported (lowercase) = private

### Ruby (.rb)
- Classes, modules (both as class symbols)
- Methods (instance and singleton `def self.method`)
- Functions (top-level `def`)
- Imports (`require`, `require_relative`)
- Function/method calls (including receiver: `obj.method`)
- Inheritance (`class Child < Parent`)
- Mixins (`include`, `extend`, `prepend` → inherits edges)
- Raise statements (`raise ExceptionClass`)
- Rescue clause type references (`rescue TypeError, KeyError`)
- Namespaced classes (`Foo::Bar`)
- Doc comments (`#` preceding declarations)
- Variable assignments (module-level and class-level)
- Visibility: public (default), `_prefixed` (private convention)

### Java (.java)
- Classes, interfaces, enums, annotation types (all as class symbols)
- Methods, constructors (as method symbols)
- Fields, constants (as variable symbols)
- Imports (named only, wildcards skipped)
- Method calls (including receiver: `obj.method`)
- `new` expressions → `References` edges
- Inheritance (`extends` → inherits edge)
- Interface implementation (`implements` → inherits edges)
- Declared throws clauses → `Raises` edges
- `throw` statements → `Raises` edges
- Annotation references (`@Override`, `@Deprecated`, etc.) → `References` edges
- Type references in method signatures (parameter types, return types, generic types)
- Nested named classes at any depth (anonymous classes skipped)
- Javadoc (`/** ... */`) and line comment (`//`) docstrings
- Visibility: `public`, `private`, `protected`; package-private (no modifier) → `Public`

### PHP (.php)
- Classes, interfaces, traits
- Methods, top-level functions
- Namespace declaration (`namespace Foo\Bar`) — used to qualify symbol names
- Use statements (`use Foo\Bar\Baz`) — resolved to FQCN for edge targets
- Inheritance (`extends` → `Inherits` edge)
- Interface implementation (`implements` → `Implements` edge)
- Trait use (`use TraitName` inside class body → `References` edge)
- Method calls (`$obj->method()`, `Class::static()` → `Calls` edges)
- Function calls → `Calls` edges
- `new ClassName()` → `References` edges
- Docblocks (`/** ... */`) and line comments (`//`) as docstrings
- Visibility: `public`, `protected`, `private`
- Uses `LANGUAGE_PHP_ONLY` parser (files starting with `<?php`, no inline HTML)

### Markdown (.md)
- Document sections chunked by heading (`#`, `##`, `###`, etc.)
- Each heading section becomes a `Document` symbol
- Large sections sub-chunked at paragraph boundaries (~1500 bytes)
- Files without headings use fixed-size paragraph chunking
- No edges — documents are indexed for semantic/keyword search only
- Preamble text before the first heading is captured as a "preamble" symbol

## Extraction Notes

Code language extractors walk the tree-sitter CST and produce:
- **Symbols**: functions, classes, methods, variables, imports
- **Edges**: calls, imports, inherits, references, raises

The Markdown extractor uses plain string splitting (no tree-sitter) to chunk documents by heading boundaries.

Edge resolution is heuristic (exact name match, scope-aware). Priority: same file > same directory > project-wide unique match.
