---
name: write-technical-prose
description: Use when drafting or editing technical documents, commit messages, pull request descriptions, or issue text, including requested drafts in chat. Not for ordinary conversation, explanations, brainstorming, or status updates.
---

# Writing Standards

Apply these standards to the requested writing deliverable. Drafting text does not authorize saving, posting, or publishing it.

## Write for the Reader

Lead with the result or decision. Use plain language, active voice, and only the detail the reader needs. Explain unfamiliar terms; omit unsupported praise and unnecessary claims.

Use paragraphs for reasoning, lists for steps or parallel items, and tables or diagrams when they clarify the content. Follow the document's existing structure. Preserve existing requirement identifiers and update affected references when editing; do not introduce a numbering or tracking scheme unless requested.

Do not add author metadata or agent attribution, including co-author trailers and generated-by footers.

## Ground Useful Claims

Use authoritative sources to support factual claims that matter to the deliverable. Cite existing sources, revisions, or run results where readers need to verify them. For a derived result, provide the command and inputs needed to reproduce it without copying information already available at the source.

Prefer a defining source or command over counts and inventories that become stale. Include a measurement only when it helps the reader, with its scope and relevant date or revision. A reproduction command does not prove a historical run occurred; use the actual result for that claim. Use event records for dates, not unrelated file timestamps. Omit optional claims that would require extra reports, artifacts, or checks.

## Keep Evidence Minimal

Apply this section only when the requested deliverable needs supporting evidence. Do not create or retain additional evidence files by default. Reference existing sources, Git, test/build results, and issue records directly. Do not create local mirrors, evidence registries, requirement ledgers, or per-task reports unless explicitly requested.

Use the established destination for requested documentation and decisions. In repositories, keep maintained prose in tracked files and necessary generated diagnostics in temporary or replaceable ignored build/output locations or a configured artifact store. Evidence requirements do not imply committing diagnostics or copying full source files, SQL, schemas, logs, or inventories into reports. Reference necessary large artifacts by location and content hash; store identical content once.

Retain additional output only for a concrete review need, unresolved diagnosis, deliberate regression record, or established release requirement. Keep the smallest useful result accessible for review and reproduction for the needed period; ignoring a file alone does not provide provenance. Retain failed-run diagnostics only for an unresolved issue or deliberate regression record, while preserving established release evidence.

Create recurring reports only when requested or required by an existing workflow, with retention and size limits set before generation. Bound successful and failed run history, and surface and correct unexpected growth without creating a separate retention register or monitoring system.

Limit expiry or replacement to task-owned generated output within scope. Preserve user-owned changes, private data, and established release evidence; these rules do not authorize blanket deletion. Keep private evidence in access-controlled locations.

## Verify Proportionately

Match verification to the deliverable's risk without weakening required behavioral checks. Reuse existing sources, tests, reports, and tools; do not add audit frameworks, validators, or checklists just to support writing. Summarize relevant checks and limitations in the task response or existing review record; completion does not require a separate report.
