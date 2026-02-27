# VHS demo tape for cartog
# Run: vhs docs/demo.tape
# Output: docs/demo.gif

Output docs/demo.gif

Require cartog

Set Shell "bash"
Set FontSize 14
Set Width 900
Set Height 500
Set Theme "Dracula"
Set Padding 20
Set TypingSpeed 40ms

# Title
Type "# cartog — code graph indexer for LLM coding agents"
Enter
Sleep 1s

# Navigate to demo project
Type "cd benchmarks/fixtures/webapp_py"
Enter
Sleep 500ms

# Step 1: Index
Type "# Step 1: Build the code graph"
Enter
Sleep 500ms
Type "cartog index ."
Enter
Sleep 2s

# Step 2: Search
Type ""
Enter
Type "# Step 2: Find symbols by name"
Enter
Sleep 500ms
Type "cartog search validate"
Enter
Sleep 3s

# Step 3: Refs
Type ""
Enter
Type "# Step 3: Who calls validate_token?"
Enter
Sleep 500ms
Type "cartog refs validate_token"
Enter
Sleep 3s

# Step 4: Impact
Type ""
Enter
Type "# Step 4: What breaks if I change it?"
Enter
Sleep 500ms
Type "cartog impact validate_token --depth 3"
Enter
Sleep 5s
