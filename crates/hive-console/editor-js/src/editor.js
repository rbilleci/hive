import { EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { LanguageDescription, LanguageSupport, StreamLanguage, bracketMatching, defaultHighlightStyle, indentOnInput, syntaxHighlighting } from "@codemirror/language";
import { markdown } from "@codemirror/lang-markdown";
import { python } from "@codemirror/lang-python";
import { javascript } from "@codemirror/lang-javascript";
import { json } from "@codemirror/lang-json";
import { xml } from "@codemirror/lang-xml";
import { shell } from "@codemirror/legacy-modes/mode/shell";

const markdownLanguages = [
  LanguageDescription.of({ name: "Python", alias: ["python", "py"], support: python() }),
  LanguageDescription.of({ name: "Shell", alias: ["shell", "sh", "bash"], support: new LanguageSupport(StreamLanguage.define(shell)) }),
  LanguageDescription.of({ name: "JSON", alias: ["json"], support: json() }),
  LanguageDescription.of({ name: "JavaScript", alias: ["javascript", "js"], support: javascript() }),
  LanguageDescription.of({ name: "TypeScript", alias: ["typescript", "ts"], support: javascript({ typescript: true }) }),
  LanguageDescription.of({ name: "XML", alias: ["xml"], support: xml() })
];

function languageExtension(language) {
  switch (language) {
    case "markdown": return markdown({ codeLanguages: markdownLanguages, completeHTMLTags: false });
    case "python": return python();
    case "shell": return StreamLanguage.define(shell);
    case "json": return json();
    case "javascript": return javascript();
    case "typescript": return javascript({ typescript: true });
    case "xml": return xml();
    default: return [];
  }
}

/** Mounts a CodeMirror view in `host`. `onChange(text)` fires for user edits only, never for `setDocument`. */
export function create(host, options, onChange) {
  const handle = { view: null, applying: false };
  const contentAttributes = { "aria-label": options.label, "aria-labelledby": options.labelledBy, "aria-describedby": options.describedBy };
  if (options.focusId) contentAttributes.id = options.focusId;
  handle.view = new EditorView({
    parent: host,
    state: EditorState.create({
      doc: options.doc,
      extensions: [
        history(),
        keymap.of([...defaultKeymap, ...historyKeymap, indentWithTab]),
        languageExtension(options.language),
        indentOnInput(),
        bracketMatching(),
        syntaxHighlighting(defaultHighlightStyle),
        EditorView.lineWrapping,
        EditorView.editable.of(!options.readOnly),
        EditorView.contentAttributes.of(contentAttributes),
        EditorView.updateListener.of((update) => {
          if (update.docChanged && !handle.applying) onChange(update.state.doc.toString());
        })
      ]
    })
  });
  return handle;
}

export function setDocument(handle, value) {
  const view = handle.view;
  if (!view || view.state.doc.toString() === value) return;
  handle.applying = true;
  try { view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: value } }); }
  finally { handle.applying = false; }
}

export function destroy(handle) {
  handle.view?.destroy();
  handle.view = null;
}
