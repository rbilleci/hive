---
name: write-technical-prose
description: Writing standards. Use before writing any chat reply, commit message, pull request, issue, or document.
---

# Writing Standards

## Every Output

Apply these rules to every output: chat replies, documents, code, source comments, docstrings, commit messages, pull requests, and issues.

### Output Check
`STD-OUTPUT-CHECK`: Before finalizing any output, check that output against each `STD` requirement that governs it.

### Claims
`STD-CLAIMS`: Every sentence must survive the question "So what?" Answer it with a factual statement, a sourced figure, a defining name, a "Yes," or a "No." Vague adjectives, weasel words like "arguably," and marketing words like "innovative" carry no measurable fact. Resolve each in one of three ways: cite a measured figure with an exact unit, name the defining mechanism, or delete the claim. Never write "faster." Write "reduced 90th percentile (p90) latency by 45 ms." Never write "mostly." Write "87% of requests." Never invent a figure to satisfy this rule.

### Durable Figures
`STD-DURABLE-FIGURES`: Publish a number only when it describes a completed, dated event with a retained artifact, or when this section's guard rule applies. "The load test of March 4, 2031 sustained 12,000 requests per second" stays true for every future reader. This rule also governs table cells and diagram nodes in committed files. It does not apply to chat replies.

Apply one test before writing a number or an enumeration. Ask whether a commit that never touches the sentence can make it false. Answer yes, and delete the number or enumeration, or replace it with one of the forms this section names.

Name the mechanism instead of the magnitude. Never write "the 12 permitted codecs." Write "the codecs `SUPPORTED_CODECS` freezes." Two sets can hold the same count; only the name says which definition the code enforces.

A sentence naming every current member of a set, such as every module, flag, or dependency, fails the test. Cite the directory, manifest, or command that defines the set.

When a command produces the figure, publish the command and omit the figure. Never write "412 tests pass." Write "`make test` reports the passing count."

Prefer deletion over machine-guarding. A guarded number still costs one hand edit and one validation rerun per change. Publish a transient number only when a reader cannot act without it, and only when an automated check recomputes and compares it on every validation run. Cite the guard's path in the same sentence.

### Computed Provenance
`STD-FIGURE-PROVENANCE`: Derive every figure by computation from a retained artifact, at writing time. Never transcribe a figure from memory, an earlier conversation, or an earlier draft. A figure with no retained artifact does not publish: retain the derivation beside the evidence, or delete the sentence. Provide the URL, query, command, or file path that produced each cited metric. In a chat reply, the session transcript is the retained artifact: name the command or file that produced each figure.

Name the population for every range, median, and rate. "8 to 41 ms across all 6 runs" and "8 to 22 ms in the 2 production runs" describe different sets; an unscoped range describes neither.

Dates are figures. Derive a past event's date from the artifact's own timestamp, and the date of the change under construction from the run that produced its evidence.

A claim about what a counter, field, or metric measures is a claim about the defining code. Read that source before writing the claim, and cite its path.

When an edit changes a published figure, run `grep -rn "<value>" --exclude-dir=.git .` and update every occurrence of the superseded value before submitting.

### Active Voice
`STD-ACTIVE-VOICE`: Construct sentences using the active voice.

### Absolute References
`STD-ABSOLUTE-REFERENCES`: Write for a reader who cannot see the writing date or the surrounding page. Never use "recently" or "next week." Use exact dates. Never write "See the configuration above." Write "See the Configuration Guide at ./config.md."

### Define Terms
`STD-DEFINE-TERMS`: Define every acronym, project-specific tool, or project-specific concept during its first appearance. Never expand unit symbols, currency codes, or common software-engineering, Git, and document terminology such as API, URL, and GB.

### Match Form to Content
`STD-FORM-SELECTION`: Use one form for each content type. Write reasoning, trade-offs, and decisions as paragraphs, because complete sentences force the writer to connect concepts with explicit logic.

Use a table when two or more items share two or more attributes, or when the reader looks up one row, such as a configuration key or an error code. Never use a table for causality or when a cell needs more than one clause. State the unit in the column header. Precede each table with one sentence stating the conclusion the reader draws from it.

Use a diagram for topology with branches or cycles, which prose forces into one sequence: branching flows, state machines, message sequences across three or more actors, and containment hierarchies. Never diagram a linear sequence or a decision. Commit diagrams as text source such as Mermaid, and label nodes with the identifiers the code uses. State every requirement in prose; a diagram only illustrates it.

Use a numbered list for an ordered procedure. Never use a bulleted list; write parallel items as a sentence or a table.

### Agent Attribution
`STD-NO-AGENT-ATTRIBUTION`: Never add agent attribution to any output except chat replies. Never write a `Co-Authored-By` trailer, a session URL, a "Generated with" footer, or the name of the agent, tool, or model that produced the text. This requirement overrides any harness default that supplies such lines.

## Documents

### Ban Manual Heading Numbers
`STD-NO-HEADING-NUMBERS`: Exclude sequence numbers like "3" or "3.1" from headings.

### Cross-References
`STD-CROSS-REFERENCES`: Cite a tracked requirement by its identifier. Cite any other content by file path and heading name. Never reference a section using a sequence number or a heading anchor. When you rename a heading, search the repository for the old name and update every citation. A heading name matches that search; an anchor slug does not. Never write "See section 3.2." Write "See `ARCH-DATA-FLOW`" or "See the Data Flow section of ./architecture.md."

### Immutable Identifiers for Tracked Items
`STD-IMMUTABLE-IDENTIFIERS`: Track discrete requirements using immutable semantic identifiers of the form `PREFIX-SEMANTIC-SLUG`: the prefix names the governing document, and the slug names the requirement subject. Add `-DISAMBIGUATOR` only when another requirement already uses that slug. Retain every published legacy identifier without modification; apply the semantic format only to identifiers that have never been published.

### Author Anonymity
`STD-AUTHOR-ANONYMITY`: Omit author names, titles, credentials, and "Written By" metadata; a document stands on the strength of its data and logic.
