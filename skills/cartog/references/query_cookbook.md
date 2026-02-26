# cartog Query Cookbook

## Common Patterns

### "Where is this symbol defined?"

Start broad, then narrow only if needed:
```bash
cartog search extract
# → 1 result? Use it. Done.
# → many results with different names? Add --kind:
cartog search extract --kind method
# → still multiple files? Add --file:
cartog search extract --kind method --file src/languages/python.rs
# → now unique: proceed with outline/refs/callees
```

### "What does this file contain?"
```bash
cartog outline src/auth/tokens.py
```

### "Who calls this function?"
```bash
cartog refs validate_token --kind calls
```

### "What does this function depend on?"
```bash
cartog callees authenticate
```

### "Is it safe to change this?"
```bash
cartog impact validate_token --depth 3
```

### "Show me the class hierarchy"
```bash
cartog hierarchy BaseService
```

### "What does this file import?"
```bash
cartog deps src/routes/auth.py
```

### "Find everything related to a name"
```bash
cartog refs SessionManager
```

## Combining Commands

### Understand a function before modifying it
```bash
cartog search validate_token           # Locate the symbol — note the file path returned
cartog outline <file-from-search>      # See structure of that file
cartog callees validate_token          # What it depends on
cartog impact validate_token           # Who depends on it
```

### Trace a call chain
```bash
cartog callees handle_request          # What does it call?
cartog callees authenticate            # What does authenticate call?
cartog callees validate_token          # Keep going deeper
```

### Assess refactoring scope
```bash
cartog search OldClassName             # Confirm exact name and file first
cartog refs OldClassName               # All references
cartog hierarchy OldClassName          # Subclasses to update
cartog impact OldClassName --depth 5   # Full blast radius
```

## Tips

- Use `--json` when you need to parse output programmatically
- After making changes, run `cartog index .` to update (uses git to detect changes)
- Use `cartog index . --force` to rebuild the entire index from scratch
- Symbol names are matched by simple name — use `validate_token`, not `auth.tokens.validate_token`
- For method queries, use the method name (e.g., `authenticate`). Dotted names like `UserService.authenticate` are accepted but resolved to the simple part
