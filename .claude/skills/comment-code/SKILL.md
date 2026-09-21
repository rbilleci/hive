---
name: comment-code
description: Code comment standards. Use when writing or editing source code, comments, docstrings, or symbol names.
---

# Code Standards

Apply `../write-technical-prose/SKILL.md` to every comment this rule permits.

## Code Comments
`STD-CODE-COMMENTS`: Write a comment or docstring only when a reader who has only the source code would misuse or break the code without it. Never write a comment that restates the code, narrates the change, or holds commented-out code. Never reference a ticket, task, pull request, document, URL, standard, or requirement identifier in a comment, docstring, or symbol name; a comment may name a symbol or code file in the same repository. State the fact the reference would have supplied, and put the reference in the commit message. Never write "// See PROJ-1234." or "// Per `STD-CLAIMS`." Write "// The upstream parser rejects a trailing comma." In source files this rule takes precedence: where another rule requires a reference this rule bans, delete the content that needs it. This rule exempts license headers and directives a tool reads.
