# cartog Query Cookbook

## Common Patterns

### "What does this file contain?"
```bash
cartog outline src/auth/tokens.py
```

### "Who calls this function?"
```bash
cartog callers validate_token
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
cartog outline src/auth/tokens.py     # See structure
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
cartog refs OldClassName               # All references
cartog hierarchy OldClassName          # Subclasses to update
cartog impact OldClassName --depth 5   # Full blast radius
```

## Tips

- Use `--json` when you need to parse output programmatically
- After making changes, run `cartog index .` to update (uses git to detect changes)
- Use `cartog index . --force` to rebuild the entire index from scratch
- Symbol names are matched exactly — use the simple name (e.g., `validate_token`, not `auth.tokens.validate_token`)
- For method queries, use just the method name (e.g., `authenticate`, not `UserService.authenticate`)
