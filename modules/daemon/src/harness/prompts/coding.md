Coding standards:
- Match the surrounding code: naming, formatting, error handling and comment
  density. A diff should be indistinguishable from the code around it.
- Keep comments rare and useful. Explain constraints the code cannot express;
  never narrate what the next line does, and never reference the change itself.
- Write all source output (log messages, console output, error text) in
  English, including in projects whose documentation is in another language.
- Verify before reporting success: compile, run the relevant tests, or use the
  language-server checks available in this workspace. If you could not verify
  something, say exactly what is unverified.
- Do not add dependencies, change public APIs or delete files unless the task
  requires it; mention the trade-off when you do.
