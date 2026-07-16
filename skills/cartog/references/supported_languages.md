# Supported Languages

## Currently Supported (16 languages + 4 frameworks)

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
- JSX component usage (`<Counter/>`, `<Foo.Bar/>`) → `Calls` edge (React); lowercase HTML and lowercase dotted host tags (`<svg.path/>`) skipped. Captured inside function/method bodies; calls and JSX in a module-scope variable initializer (e.g. `const x = <Counter/>;`) are not attributed to an enclosing scope (same as a top-level `const x = foo();`).
- Async functions
- JSDoc comments
- Class fields with visibility (public/private/protected TS modifiers, #private, _convention)

### JavaScript (.js, .jsx, .mjs, .cjs)
- Functions (declaration and arrow), classes, methods
- Imports (ES modules)
- Function calls, `new` expressions, throw statements
- Inheritance (extends)
- JSX component usage (`<Counter/>`, `<Foo.Bar/>`) → `Calls` edge (React); lowercase HTML elements skipped
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

### C# (.cs)
- Classes, structs, records (all as class symbols), interfaces, enums (with members as variable symbols), delegates (as function symbols)
- Methods, constructors (as method symbols), properties (as variable symbols), fields, record positional parameters
- Namespaces (file-scoped `namespace Foo;` and block `namespace Foo { }`) — used to qualify symbol names, not emitted as symbols
- Using directives (`using`, `global using`, `using static`, `using Alias = ...`) → `Imports` edges
- Method calls (including receiver: `obj.Method()`) and top-level statements (implicit entry point)
- `new` expressions → `References` edges
- Base list: interfaces → `Inherits`; classes/structs/records → `Implements` for every entry (the grammar cannot distinguish a base class from an interface — "extends" is recovered from the resolved target's kind). Consequence: C# class inheritance shows up in `cartog hierarchy` but not in `refs <base> --kind inherits` (it's stored as an `implements` edge); use `hierarchy` for C# inheritance trees.
- Type references in method/property signatures (parameter types, return types, generic type arguments)
- Nested types at any depth; partial classes as separate per-file symbols
- Async methods (`async` modifier)
- XML-doc comments (`/// <summary>...`) as docstrings (tags stripped)
- Visibility: `public`, `private`, `protected`; `internal`/no modifier → `Public`

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

### Dart (.dart)
- Classes (including abstract and `sealed`), mixins (as trait symbols), extensions (as class symbols), enums, typedefs
- Top-level functions, methods, constructors (default/named/factory/const), getters/setters
- Imports (`import 'package:foo/bar.dart'`, `dart:async`, relative paths), part directives
- Function and method calls
- Inheritance (`extends` and `with` mixins → `Inherits` edges)
- Interface implementation (`implements` → `Implements` edges)
- Type annotation references (parameter types, return types, generic types)
- Async functions / methods
- Doc comments (`///`)
- Library-private visibility (leading underscore `_foo` → private)

### Swift (.swift)
- Classes, structs, actors (all as class symbols), protocols (as interfaces), enums, extensions (as class symbols), typealiases/associatedtypes
- Top-level functions, methods, `init`/`deinit`/`subscript`, properties (`let`/`var`), enum cases
- Imports (`import Foundation`, `import UIKit.UIView`)
- Function and method calls
- Inheritance (class superclass → `Inherits`) and conformance (protocols → `Implements`)
- Type annotation references (parameter types, return types, generic types)
- Async functions (`async`); `throws`/`rethrows` kept in the signature (no `Raises` edge — Swift errors are untyped at the declaration site)
- Doc comments (`///` lines and `/** */` blocks)
- Visibility: `private`/`fileprivate` → private; `internal` (default)/`public`/`open`/`package` → public

### Kotlin (.kt, .kts)
- Classes, `data`/`sealed` classes, interfaces (as interfaces), `enum class` (as enums), `object`/`companion object` (as class symbols), typealiases
- Top-level functions, methods, primary/secondary constructors (as `init` methods), properties (`val`/`var`), constructor `val`/`var` parameters, enum entries
- Imports (`import a.b.C`, `import a.b.*`, `import a.b.C as D`)
- Function and method calls
- Inheritance (superclass constructor call → `Inherits`) and interface implementation (→ `Implements`)
- Type annotation references (parameter types, return types, generic type arguments)
- Coroutine functions (`suspend` → marked async)
- KDoc comments (`/** */` blocks)
- Visibility: `private` → private; `internal`/`protected`/`public` (default) → public
- Annotations (`@Deprecated`, `@Override`) are not emitted as type references

### Vue (.vue)
- Symbols + edges from every `<script>` / `<script setup>` block (one file may have several)
- `lang="ts"` / `lang="typescript"` → TypeScript extractor; otherwise JavaScript
- Same symbols/edges as the delegated JS/TS extractor (functions, classes, methods, variables, imports; calls, imports, inherits, type refs)
- Byte/line offsets remapped back to the full `.vue` file
- Template markup and `<style>` blocks are not indexed

### Svelte (.svelte)
- Symbols + edges from the `<script>` block(s), including `<script context="module">`
- `lang="ts"` → TypeScript extractor; otherwise JavaScript
- Same symbols/edges as the delegated JS/TS extractor
- Markup and `<style>` blocks are not indexed

### Astro (.astro)
- Symbols + edges from the `---` frontmatter (always TypeScript) plus any client-side `<script>` blocks
- Same symbols/edges as the delegated JS/TS extractor
- HTML/template markup is not indexed

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
