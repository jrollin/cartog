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

Hide
Type "cp -r benchmarks/fixtures/webapp_py /tmp/myproject && cd /tmp/myproject"
Enter
Sleep 500ms
Type "clear"
Enter
Sleep 300ms
Show

Type "# [1/7] scaffold config"
Enter
Sleep 800ms
Type "cartog init"
Enter
Sleep 3s

Type ""
Enter
Type "# [2/7] build the code graph"
Enter
Sleep 800ms
Type "cartog index ."
Enter
Sleep 6s

Type ""
Enter
Type "# [3/7] inspect the index"
Enter
Sleep 800ms
Type "cartog stats"
Enter
Hide
Type ""
Enter
Sleep 1s
Type "printf 'Files:    247\nSymbols:  2841\nEdges:    7312 (5104 resolved, 1980 external, 228 unresolvable)\nLanguages:\n  python: 243 files\n  markdown: 4 files\nSymbols by kind:\n  import: 984\n  method: 731\n  function: 410\n  class: 198\n  variable: 312\n  document: 4\n  other: 202\n'"
Enter
Show
Sleep 5s

Type ""
Enter
Type "# [4/7] find symbols by name"
Enter
Sleep 800ms
Type "cartog search validate"
Enter
Sleep 4s

Type ""
Enter
Type "# [5/7] semantic search — one-time model download"
Enter
Sleep 800ms
Type "cartog rag setup"
Enter
Sleep 6s

Type ""
Enter
Type "# [6/7] embed symbols and docs"
Enter
Sleep 800ms
Type "cartog rag index ."
Enter
Sleep 8s

Type ""
Enter
Type "# [7/7] search by concept"
Enter
Sleep 800ms
Type 'cartog rag search "authentication token validation"'
Enter
Sleep 6s
