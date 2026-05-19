# VHS demo tape for cartog
# Run: vhs docs/demo.tape
# Output: docs/demo.gif

Output docs/demo.gif

Require cartog

Set Shell "bash"
Set FontSize 14
Set Width 900
Set Height 560
Set Theme "Dracula"
Set Padding 20
Set TypingSpeed 40ms

# Title
Type "# cartog — code graph indexer for LLM coding agents"
Enter
Sleep 1s

# Navigate to a fresh demo project (clean slate so init shows real output)
Type "cd benchmarks/fixtures/webapp_py"
Enter
Sleep 500ms
Type "rm -rf .cartog .cartog.toml .mcp.json .cursor .vscode 2>/dev/null"
Enter
Sleep 300ms
Type "clear"
Enter
Sleep 300ms

# Two-step bootstrap: init + index
Type "# Two commands to get started"
Enter
Sleep 400ms

Type "cartog init"
Enter
Sleep 3s

Type ""
Enter
Type "cartog index ."
Enter
Sleep 3s

# Codebase map — orientation
Type ""
Enter
Type "# Orient yourself: file tree + top symbols"
Enter
Sleep 400ms
Type "cartog map --tokens 1500"
Enter
Sleep 3s

# Keyword search
Type ""
Enter
Type "# Find symbols by name (sub-millisecond)"
Enter
Sleep 400ms
Type "cartog search validate"
Enter
Sleep 3s

# Semantic search
Type ""
Enter
Type "# Find code by concept (semantic search)"
Enter
Sleep 400ms
Type 'cartog rag search "authentication token validation"'
Enter
Sleep 4s

# Refs
Type ""
Enter
Type "# Who calls validate_token?"
Enter
Sleep 400ms
Type "cartog refs validate_token --kind calls"
Enter
Sleep 3s

# Impact
Type ""
Enter
Type "# What breaks if I change it?"
Enter
Sleep 400ms
Type "cartog impact validate_token --depth 3"
Enter
Sleep 4s

# Optional: editor integration
Type ""
Enter
Type "# Optional — wire cartog into your editor for AI agents"
Enter
Sleep 400ms
Type "cartog ide --scope project --yes"
Enter
Sleep 4s
