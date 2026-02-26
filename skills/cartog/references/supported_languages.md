# Supported Languages

## Currently Supported

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

## Planned

### Java (.java)

## Extraction Notes

Each language extractor walks the tree-sitter CST and produces:
- **Symbols**: functions, classes, methods, variables, imports
- **Edges**: calls, imports, inherits, references, raises

Edge resolution is heuristic (exact name match, scope-aware). Priority: same file > same directory > project-wide unique match.
