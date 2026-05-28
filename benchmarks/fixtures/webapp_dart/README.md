# webapp_dart

Synthetic Dart fixture for cartog benchmarks. Covers each Dart paradigm in a
small, self-contained project: classes, abstract classes, mixins, extensions,
enums, typedefs, generics, async/await, library/part splits, package imports,
and library-private visibility (leading underscore).

## Validate

```bash
dart analyze
```

The Makefile `check-dart` target runs this inside the `dart:stable` Docker image.
