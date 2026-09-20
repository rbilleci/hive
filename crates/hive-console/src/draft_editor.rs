//! Ports `AgentDraftEditor.tsx`: structured inputs stay native; only source fields use CodeMirror.

use crate::agent_tabs::AgentTabs;
use crate::api::agent_draft::{
    object_at, publish_agent_draft, request_agent_draft, request_agent_draft_review,
    save_agent_draft, text_at, validate_agent_draft, AgentDraftDiagnostic, AgentDraftFields,
    AgentDraftMutation, AgentDraftProblemFields, AgentDraftReviewFields, DraftDocument,
};
use crate::api::configuration::{request_known_references, ReferenceOption};
use crate::code_editor::{supported_language, CodeEditor, SafeMarkdownPreview};
use crate::confirmation_dialog::ConfirmationDialog;
use crate::graphql::GraphqlError;
use crate::navigation_guard::use_navigation_guard;
use crate::page_header::PageHeader;
use crate::shell::use_console;
use leptos::ev;
use leptos::html::{Aside, Button};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::Value;
use wasm_bindgen::JsCast;

const SECTIONS: [(&str, &str); 15] = [
    ("General", "general"),
    ("Instructions", "instructions"),
    ("Harness", "harness"),
    ("Model", "model"),
    ("Tools", "tools"),
    ("Skills", "skills"),
    ("Capabilities", "capabilities"),
    ("Subagents", "subagents"),
    ("Memory", "memory"),
    ("Guardrails", "guardrails"),
    ("Identity", "identity"),
    ("Observability", "observability"),
    ("Limits", "limits"),
    ("Evaluations", "evaluations"),
    ("Review", "review"),
];

/// The sections edited as source text: their editor label and default language.
fn rich_section(key: &str) -> Option<(&'static str, &'static str)> {
    Some(match key {
        "instructions" => ("System instructions", "markdown"),
        "harness" => ("Harness source", "python"),
        "tools" => ("Tool configuration source", "json"),
        "skills" => ("Skills source", "javascript"),
        "capabilities" => ("Capabilities source", "typescript"),
        "guardrails" => ("Guardrails source", "shell"),
        "observability" => ("Observability source", "xml"),
        _ => return None,
    })
}

fn key_for(label: &str) -> &'static str {
    SECTIONS
        .iter()
        .find(|(name, _)| *name == label)
        .map_or("review", |(_, key)| key)
}

fn label_for(key: &str) -> &'static str {
    SECTIONS
        .iter()
        .find(|(_, candidate)| *candidate == key)
        .map_or("Review", |(name, _)| name)
}

/// `NOT_VALIDATED` becomes `Not_Validated`, exactly as the React console renders it.
fn title(value: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for letter in value.to_lowercase().chars() {
        out.push(if upper {
            letter.to_ascii_uppercase()
        } else {
            letter
        });
        upper = letter == '_';
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Loading,
    Loaded,
    SessionError,
    Unavailable,
    Error,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SaveState {
    Saved,
    Saving,
    Unsaved,
    Conflict,
    Error,
}

impl SaveState {
    fn message(self) -> &'static str {
        match self {
            Self::Saved => "Saved",
            Self::Saving => "Saving",
            Self::Unsaved => "Unsaved",
            Self::Conflict => "Conflict",
            Self::Error => "Error",
        }
    }
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn focusable(panel: &web_sys::Element) -> Vec<web_sys::HtmlElement> {
    let selector = "button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex=\"-1\"])";
    let Ok(nodes) = panel.query_selector_all(selector) else {
        return Vec::new();
    };
    (0..nodes.length())
        .filter_map(|index| nodes.item(index)?.dyn_into::<web_sys::HtmlElement>().ok())
        .filter(|element| !element.has_attribute("hidden"))
        .collect()
}

#[component]
pub fn AgentDraftEditor() -> impl IntoView {
    let params = use_params_map();
    let project_id = Memo::new(move |_| params.read().get("project_id").unwrap_or_default());
    let agent_id = Memo::new(move |_| params.read().get("agent_id").unwrap_or_default());
    let navigate = use_navigate();
    let leave_navigate = navigate.clone();
    let console = use_console();
    let revision = console.revision;

    let draft = RwSignal::new(None::<AgentDraftFields>);
    let document = RwSignal::new(DraftDocument::new());
    let selected = RwSignal::new("General");
    let screen = RwSignal::new(Screen::Loading);
    let save_state = RwSignal::new(SaveState::Saved);
    let problem = RwSignal::new(None::<AgentDraftProblemFields>);
    let validating = RwSignal::new(false);
    let review = RwSignal::new(None::<AgentDraftReviewFields>);
    let reviewing = RwSignal::new(false);
    let warnings_acknowledged = RwSignal::new(false);
    let context_open = RwSignal::new(false);
    let reload_attempt = RwSignal::new(0_u32);
    // A response is applied only while the load that was current at dispatch still is.
    let generation = StoredValue::new(0_u32);
    let current = move |expected: u32| generation.try_get_value() == Some(expected);
    // `None` while the options load; the catalog and the project's immutable resource versions.
    let references = RwSignal::new(None::<Vec<ReferenceOption>>);
    let context_panel = NodeRef::<Aside>::new();
    let context_trigger = NodeRef::<Button>::new();

    Effect::new(move |_| {
        let (project, agent) = (project_id.get(), agent_id.get());
        reload_attempt.track();
        // Changed access reloads the draft: a revoked editor must see the read-only page.
        let _ = revision.get();
        let request = generation.get_value() + 1;
        generation.set_value(request);
        screen.set(Screen::Loading);
        draft.set(None);
        review.set(None);
        problem.set(None);
        selected.set("General");
        save_state.set(SaveState::Saved);
        spawn_local(async move {
            let result = request_agent_draft(&project, &agent).await;
            if !current(request) {
                return;
            }
            match result {
                Ok(Some(value)) => {
                    document.set(value.document());
                    draft.set(Some(value));
                    screen.set(Screen::Loaded);
                }
                Ok(None) => screen.set(Screen::Unavailable),
                Err(GraphqlError::SessionExpired) => screen.set(Screen::SessionError),
                Err(GraphqlError::Transport(_)) => screen.set(Screen::Error),
            }
        });
    });

    Effect::new(move |_| {
        let project = project_id.get();
        let organization = console.context.with(|context| {
            context
                .organizations
                .iter()
                .find(|organization| {
                    organization
                        .projects
                        .iter()
                        .any(|entry| entry.id.inner() == project)
                })
                .map(|organization| organization.id.inner().to_string())
        });
        let Some(organization) = organization else {
            references.set(Some(Vec::new()));
            return;
        };
        references.set(None);
        spawn_local(async move {
            let known = request_known_references(&project, &organization)
                .await
                .unwrap_or_default();
            if project_id
                .try_get_untracked()
                .is_some_and(|current| current == project)
            {
                references.set(Some(known));
            }
        });
    });

    let guard = use_navigation_guard(Signal::derive(move || save_state.get() != SaveState::Saved));

    let unload_guard = window_event_listener(ev::beforeunload, move |event| {
        if save_state
            .try_get_untracked()
            .is_some_and(|state| state != SaveState::Saved)
        {
            event.prevent_default();
            event.set_return_value("");
        }
    });
    on_cleanup(move || unload_guard.remove());

    let close_context = move |return_focus: bool| {
        context_open.set(false);
        if return_focus {
            request_animation_frame(move || {
                if let Some(trigger) = context_trigger.get_untracked() {
                    let _ = trigger.focus();
                }
            });
        }
    };

    // While the diagnostics dialog is open: focus its first control, contain Tab, close on Escape.
    Effect::new(move |_| {
        if !context_open.get() {
            return;
        }
        request_animation_frame(move || {
            if let Some(first) = context_panel
                .get_untracked()
                .and_then(|panel| focusable(&panel).into_iter().next())
            {
                let _ = first.focus();
            }
        });
        let trap = window_event_listener(ev::keydown, move |event| {
            if event.key() == "Escape" {
                event.prevent_default();
                close_context(true);
                return;
            }
            if event.key() != "Tab" {
                return;
            }
            let Some(panel) = context_panel.get_untracked() else {
                return;
            };
            let targets = focusable(&panel);
            let (Some(first), Some(last)) = (targets.first(), targets.last()) else {
                event.prevent_default();
                return;
            };
            let active = document_active_element();
            let index = targets.iter().position(|target| {
                active
                    .as_ref()
                    .is_some_and(|element| element == target.as_ref())
            });
            if event.shift_key() && index.is_none_or(|index| index == 0) {
                event.prevent_default();
                let _ = last.focus();
            } else if !event.shift_key() && index == Some(targets.len() - 1) {
                event.prevent_default();
                let _ = first.focus();
            }
        });
        on_cleanup(move || trap.remove());
    });

    let change_document = move |next: DraftDocument| {
        document.set(next);
        review.set(None);
        problem.set(None);
        save_state.set(SaveState::Unsaved);
    };
    let update_field = move |section: &'static str, field: &'static str, value: Value| {
        let mut next = document.get_untracked();
        let mut object = object_at(&next, section);
        object.insert(field.to_string(), value);
        next.insert(section.to_string(), Value::Object(object));
        change_document(next);
    };
    let update_dependency = move |value: String, selected: bool| {
        let mut next = document.get_untracked();
        let mut current = string_list(next.get("dependencies"));
        current.retain(|item| *item != value);
        if selected {
            current.push(value);
        }
        current.sort();
        next.insert("dependencies".to_string(), Value::from(current));
        change_document(next);
    };
    // The chosen model is also the draft's one `model:` dependency.
    let update_model = move |value: String| {
        let mut next = document.get_untracked();
        let mut model = object_at(&next, "model");
        model.insert("reference".to_string(), Value::String(value.clone()));
        next.insert("model".to_string(), Value::Object(model));
        let mut dependencies = string_list(next.get("dependencies"));
        dependencies.retain(|item| !item.starts_with("model:") && *item != value);
        if !value.is_empty() {
            dependencies.push(value);
        }
        dependencies.sort();
        next.insert("dependencies".to_string(), Value::from(dependencies));
        change_document(next);
    };
    let select_section = move |label: &'static str| {
        selected.set(label);
        close_context(false);
    };

    let apply_refusal = move |result: &AgentDraftMutation| -> bool {
        let Some(first) = result.problems.first() else {
            return false;
        };
        save_state.set(if first.code == "REVISION_CONFLICT" {
            SaveState::Conflict
        } else {
            SaveState::Error
        });
        problem.set(Some(first.clone()));
        true
    };

    let save_now = move || {
        let Some(revision) =
            draft.with_untracked(|value| value.as_ref().map(|value| value.revision))
        else {
            return;
        };
        if save_state.get_untracked() == SaveState::Saving {
            return;
        }
        let (project, agent, snapshot, request) = (
            project_id.get_untracked(),
            agent_id.get_untracked(),
            document.get_untracked(),
            generation.get_value(),
        );
        save_state.set(SaveState::Saving);
        problem.set(None);
        spawn_local(async move {
            let result = save_agent_draft(&project, &agent, revision, &snapshot).await;
            if !current(request) {
                return;
            }
            match result {
                Err(GraphqlError::SessionExpired) => screen.set(Screen::SessionError),
                Err(GraphqlError::Transport(_)) => save_state.set(SaveState::Error),
                Ok(result) => {
                    if apply_refusal(&result) {
                        return;
                    }
                    let Some(saved) = result.agent_draft else {
                        save_state.set(SaveState::Error);
                        return;
                    };
                    // An edit made while the request was in flight stays local and stays saveable.
                    if document.with_untracked(|value| *value == snapshot) {
                        document.set(saved.document());
                        save_state.set(SaveState::Saved);
                    } else {
                        save_state.set(SaveState::Unsaved);
                    }
                    draft.set(Some(saved));
                }
            }
        });
    };

    let validate_saved_draft = move || {
        let Some(revision) =
            draft.with_untracked(|value| value.as_ref().map(|value| value.revision))
        else {
            return;
        };
        if save_state.get_untracked() != SaveState::Saved || validating.get_untracked() {
            return;
        }
        let (project, agent, snapshot, request) = (
            project_id.get_untracked(),
            agent_id.get_untracked(),
            document.get_untracked(),
            generation.get_value(),
        );
        validating.set(true);
        problem.set(None);
        spawn_local(async move {
            let result = validate_agent_draft(&project, &agent, revision).await;
            if !current(request) {
                return;
            }
            match result {
                Err(GraphqlError::SessionExpired) => {
                    screen.set(Screen::SessionError);
                    return;
                }
                Err(GraphqlError::Transport(_)) => save_state.set(SaveState::Error),
                Ok(result) => {
                    if !apply_refusal(&result) {
                        if let Some(validated) = result.agent_draft {
                            draft.set(Some(validated));
                            save_state.set(
                                if document.with_untracked(|value| *value == snapshot) {
                                    SaveState::Saved
                                } else {
                                    SaveState::Unsaved
                                },
                            );
                        }
                    }
                }
            }
            validating.set(false);
        });
    };

    let load_review = move || {
        if save_state.get_untracked() != SaveState::Saved {
            return;
        }
        let (project, agent) = (project_id.get_untracked(), agent_id.get_untracked());
        reviewing.set(true);
        spawn_local(async move {
            match request_agent_draft_review(&project, &agent).await {
                Ok(Some(value)) => review.set(Some(value)),
                Ok(None) => screen.set(Screen::Unavailable),
                Err(GraphqlError::SessionExpired) => screen.set(Screen::SessionError),
                Err(GraphqlError::Transport(_)) => problem.set(Some(AgentDraftProblemFields {
                    code: "REVIEW_UNAVAILABLE".to_string(),
                    message: "We could not prepare this review.".to_string(),
                })),
            }
            reviewing.set(false);
        });
    };

    let publish = move || {
        let Some((revision, can_publish)) = draft.with_untracked(|value| {
            value
                .as_ref()
                .map(|value| (value.revision, value.can_publish))
        }) else {
            return;
        };
        if save_state.get_untracked() != SaveState::Saved || !can_publish {
            return;
        }
        if review.with_untracked(Option::is_none) {
            select_section("Review");
            return;
        }
        let (project, agent, acknowledged, navigate) = (
            project_id.get_untracked(),
            agent_id.get_untracked(),
            warnings_acknowledged.get_untracked(),
            navigate.clone(),
        );
        spawn_local(async move {
            match publish_agent_draft(&project, &agent, revision, acknowledged).await {
                Err(GraphqlError::SessionExpired) => screen.set(Screen::SessionError),
                Err(GraphqlError::Transport(_)) => {}
                Ok(result) => {
                    if let Some(first) = result.problems.first() {
                        problem.set(Some(first.clone()));
                        select_section("Review");
                    } else if let Some(version) = result.agent_version {
                        navigate(
                            &format!(
                                "/projects/{project}/agents/{agent}/versions/{}",
                                version.id.inner()
                            ),
                            Default::default(),
                        );
                    }
                }
            }
        });
    };

    let focus_diagnostic = move |diagnostic: &AgentDraftDiagnostic| {
        let path_section = diagnostic.path.first().map_or("review", String::as_str);
        let path_field = diagnostic.path.get(1).map(String::as_str);
        let target_section = if path_field == Some("dependencies") {
            "Model"
        } else {
            label_for(path_section)
        };
        let section_id = format!("agent-draft-section-{}", key_for(target_section));
        let target_id = match path_field {
            Some("dependencies") => "agent-draft-field-model-dependencies".to_string(),
            Some(field) => format!("agent-draft-field-{}-{field}", key_for(target_section)),
            None => section_id.clone(),
        };
        selected.set(target_section);
        close_context(false);
        // Two frames: one for the section to render, one for the target inside it to exist.
        request_animation_frame(move || {
            request_animation_frame(move || {
                let page = leptos::prelude::document();
                if let Some(target) = page
                    .get_element_by_id(&target_id)
                    .or_else(|| page.get_element_by_id(&section_id))
                {
                    let _ = target.unchecked_into::<web_sys::HtmlElement>().focus();
                }
            })
        });
    };

    let diagnostics = Memo::new(move |_| {
        review
            .with(|value| value.as_ref().map(|value| value.diagnostics.clone()))
            .unwrap_or_else(|| {
                draft.with(|value| {
                    value
                        .as_ref()
                        .map(|value| value.validation_diagnostics.clone())
                        .unwrap_or_default()
                })
            })
    });
    let error_count = move |key: &'static str| {
        diagnostics.with(|all| {
            all.iter()
                .filter(|item| {
                    item.severity == "ERROR"
                        && item.path.first().map_or("review", String::as_str) == key
                })
                .count()
        })
    };
    let general_text = move |field: &'static str| {
        document.with(|value| text_at(&object_at(value, "general"), field))
    };

    let section_panel = move || {
        let label = selected.get();
        let key = key_for(label);
        if let Some((editor_label, default_language)) = rich_section(key) {
            let section = Memo::new(move |_| document.with(|value| object_at(value, key)));
            let mode = Memo::new(move |_| {
                section.with(|value| {
                    supported_language(
                        value.get("language").and_then(Value::as_str),
                        default_language,
                    )
                })
            });
            let source = Memo::new(move |_| {
                section.with(|value| {
                    value
                        .get("source")
                        .and_then(Value::as_str)
                        .or_else(|| value.get("system").and_then(Value::as_str))
                        .unwrap_or_default()
                        .to_string()
                })
            });
            let is_markdown = Memo::new(move |_| mode.get() == "markdown");
            let editor = move || {
                view! {
                    <CodeEditor label=editor_label value=source language=mode
                        focus_id=format!("agent-draft-field-{key}-source") language_select_id=format!("agent-draft-field-{key}-language")
                        on_change=Callback::new(move |text: String| update_field(key, "source", Value::String(text)))
                        on_language_change=Callback::new(move |language: String| update_field(key, "language", Value::String(language))) />
                }
            };
            return view! {
                <div>{move || if is_markdown.get() {
                    view! { <div class="source-preview-split">{editor()}<SafeMarkdownPreview source=source /></div> }.into_any()
                } else {
                    view! { {editor()}<p role="status">"Source only: formatted preview is available for Markdown files only."</p> }.into_any()
                }}</div>
            }.into_any();
        }
        let flag = move |field: &'static str| {
            document.with(|value| object_at(value, key).get(field) == Some(&Value::Bool(true)))
        };
        match key {
            "general" => view! {
                <fieldset>
                    <label for="agent-draft-field-general-displayName">"Display name"</label>
                    <input id="agent-draft-field-general-displayName" prop:value=move || general_text("displayName")
                        on:input=move |event| update_field("general", "displayName", Value::String(event_target_value(&event))) />
                    <label for="agent-draft-field-general-description">"Description"</label>
                    <textarea id="agent-draft-field-general-description" prop:value=move || general_text("description")
                        on:input=move |event| update_field("general", "description", Value::String(event_target_value(&event))) />
                    <p>"ID: "{move || draft.with(|value| value.as_ref().map(|value| value.slug.clone()))}</p>
                </fieldset>
            }.into_any(),
            "model" => {
                let models = move || references.get().unwrap_or_default().into_iter().filter(|option| option.kind == "model").collect::<Vec<_>>();
                let reference = move || document.with(|value| text_at(&object_at(value, "model"), "reference"));
                let dependencies = move || document.with(|value| string_list(value.get("dependencies")));
                view! {
                    <fieldset>
                        <label for="agent-draft-field-model-reference">"Approved model"</label>
                        <select id="agent-draft-field-model-reference" prop:value=reference on:change=move |event| update_model(event_target_value(&event))>
                            <option value="">"No model selected"</option>
                            {move || models().into_iter().map(|option| { let value = option.value.clone(); view! { <option value=option.value selected=move || reference() == value>{option.label}</option> } }).collect_view()}
                        </select>
                        {move || match references.get() {
                            None => Some(view! { <p role="status">"Loading approved models…"</p> }),
                            Some(_) if models().is_empty() => Some(view! { <p role="status">"No approved models are available."</p> }),
                            Some(_) => None,
                        }}
                        <fieldset id="agent-draft-field-model-dependencies" class="selection-fieldset"><legend>"Typed dependencies"</legend>
                            {move || { let all = references.get().unwrap_or_default();
                                if all.is_empty() { return view! { <p role="status">"No selectable project or catalog dependencies are available."</p> }.into_any(); }
                                all.into_iter().map(|option| { let (checked, toggled) = (option.value.clone(), option.value.clone()); view! {
                                    <label><input type="checkbox" prop:checked=move || dependencies().contains(&checked)
                                        on:change=move |event| update_dependency(toggled.clone(), event_target_checked(&event)) /><span>{option.label}</span></label>
                                } }).collect_view().into_any() }}
                        </fieldset>
                    </fieldset>
                }.into_any()
            }
            "subagents" => view! {
                <fieldset><label><input id="agent-draft-field-subagents-enabled" type="checkbox" prop:checked=move || flag("enabled")
                    on:change=move |event| update_field("subagents", "enabled", Value::Bool(event_target_checked(&event))) />" Allow bounded subagents"</label></fieldset>
            }.into_any(),
            "memory" => view! {
                <fieldset><label for="agent-draft-field-memory-strategy">"Memory strategy"</label>
                    <select id="agent-draft-field-memory-strategy"
                        prop:value=move || document.with(|value| { let strategy = text_at(&object_at(value, "memory"), "strategy"); if strategy.is_empty() { "project".to_string() } else { strategy } })
                        on:change=move |event| update_field("memory", "strategy", Value::String(event_target_value(&event)))>
                        <option value="project">"Project scoped"</option><option value="none">"No retained memory"</option></select></fieldset>
            }.into_any(),
            "identity" => view! {
                <fieldset><label for="agent-draft-field-identity-persona">"Persona"</label>
                    <input id="agent-draft-field-identity-persona" prop:value=move || document.with(|value| text_at(&object_at(value, "identity"), "persona"))
                        on:input=move |event| update_field("identity", "persona", Value::String(event_target_value(&event))) /></fieldset>
            }.into_any(),
            "limits" => view! {
                <fieldset><label for="agent-draft-field-limits-maxTokens">"Maximum tokens"</label>
                    <input id="agent-draft-field-limits-maxTokens" type="number" min="1"
                        prop:value=move || document.with(|value| object_at(value, "limits").get("maxTokens").and_then(Value::as_f64).map(|number| number.to_string()).unwrap_or_default())
                        on:input=move |event| update_field("limits", "maxTokens", serde_json::Number::from_f64(event_target_value(&event).parse().unwrap_or(0.0)).map_or(Value::Null, Value::Number)) /></fieldset>
            }.into_any(),
            "evaluations" => view! {
                <fieldset><label><input id="agent-draft-field-evaluations-required" type="checkbox" prop:checked=move || flag("required")
                    on:change=move |event| update_field("evaluations", "required", Value::Bool(event_target_checked(&event))) />" Require local evaluation before deployment requests"</label></fieldset>
            }.into_any(),
            _ => view! {
                <ReviewPanel review=review reviewing=reviewing warnings_acknowledged=warnings_acknowledged
                    can_publish=Signal::derive(move || draft.with(|value| value.as_ref().is_some_and(|value| value.can_publish)))
                    on_load=Callback::new(move |()| load_review()) on_publish=Callback::new({ let publish = publish.clone(); move |()| publish() }) />
            }.into_any(),
        }
    };

    let editor = move || {
        let status = move || {
            if validating.get() {
                "Validating saved draft…"
            } else {
                save_state.get().message()
            }
        };
        let meta = move || {
            draft.with(|value| {
                value.as_ref().map(|value| {
                    format!(
                        "ID {} · Draft revision {} · Latest version {}",
                        value.slug,
                        value.revision,
                        value
                            .latest_version
                            .map_or("None".to_string(), |number| number.to_string())
                    )
                })
            })
        };
        let validation = move || {
            draft.with(|value| {
                value
                    .as_ref()
                    .map(|value| title(&value.validation_status))
                    .unwrap_or_default()
            })
        };
        let conflict = move || {
            problem.with(|value| {
                value
                    .as_ref()
                    .is_some_and(|value| value.code == "REVISION_CONFLICT")
            })
        };
        let section_panel = section_panel.clone();
        view! {
            <main class="agent-draft-editor" aria-labelledby="agent-draft-title">
                <AgentTabs project_id=project_id.get_untracked() agent_id=agent_id.get_untracked() active="draft" />
                <div class="agent-draft-content">
                    <div class="agent-draft-workspace" aria-hidden=move || (context_open.get() || guard.blocked()).then_some("true") inert=move || context_open.get() || guard.blocked()>
                        <PageHeader title_id="agent-draft-title"
                            title=Signal::derive(move || draft.with(|value| value.as_ref().map(|value| value.display_name.clone()).unwrap_or_default()))
                            meta=Signal::derive(move || meta().unwrap_or_default())>
                            <div class="agent-draft-actions">
                                <p role="status" aria-live="polite"><span>{status}</span>" · "{validation}</p>
                                <button type="button" on:click=move |_| save_now()
                                    disabled=move || matches!(save_state.get(), SaveState::Saved | SaveState::Saving) || validating.get()>
                                    {move || if save_state.get() == SaveState::Saving { "Saving draft…" } else { "Save now" }}</button>
                                <button type="button" on:click=move |_| validate_saved_draft()
                                    disabled=move || save_state.get() != SaveState::Saved || validating.get()>"Validate saved draft"</button>
                                <button type="button" on:click=move |_| select_section("Review") disabled=move || save_state.get() != SaveState::Saved>"Open review"</button>
                                <a href=move || format!("/projects/{}/agents/{}/versions", project_id.get(), agent_id.get())>"Immutable version history"</a>
                            </div>
                        </PageHeader>
                        <Show when=move || save_state.get() == SaveState::Error || problem.with(Option::is_some)>
                            <section class="agent-draft-error" role="alert">
                                <Show when=conflict><h2>"Draft revision conflict"</h2></Show>
                                <p>{move || problem.with(|value| value.as_ref().map(|value| value.message.clone()))
                                    .unwrap_or_else(|| "We could not save your changes. Your local draft is still available.".to_string())}</p>
                                <Show when=conflict><button type="button" on:click=move |_| reload_attempt.update(|value| *value += 1)>"Discard local changes and reload server version"</button></Show>
                            </section>
                        </Show>
                        <div class="agent-draft-layout">
                            <nav aria-label="Agent draft sections"><ol class="agent-draft-sections">
                                {SECTIONS.iter().map(|(label, key)| view! {
                                    <li><button id=format!("agent-draft-section-{key}") type="button"
                                        aria-current=move || (selected.get() == *label).then_some("page") on:click=move |_| select_section(label)>
                                        {*label}{move || match error_count(key) { 0 => String::new(), count => format!(" ({count})") }}</button></li>
                                }).collect_view()}
                            </ol></nav>
                            <section class="agent-draft-panel" aria-labelledby="agent-draft-section-title">
                                <h2 id="agent-draft-section-title">{move || selected.get()}</h2>
                                {section_panel}
                            </section>
                            <button node_ref=context_trigger class="agent-draft-context-trigger" type="button" aria-expanded=move || context_open.get().to_string()
                                aria-haspopup="dialog" aria-controls="agent-draft-diagnostics-panel" on:click=move |_| context_open.set(true)>"Open diagnostics"</button>
                        </div>
                    </div>
                    <Show when=move || context_open.get()>
                        <button class="agent-draft-context-backdrop" type="button" aria-label="Close diagnostics" on:click=move |_| close_context(true)></button>
                    </Show>
                    <aside node_ref=context_panel id="agent-draft-diagnostics-panel" class=move || if context_open.get() { "agent-draft-context is-open" } else { "agent-draft-context" }
                        role=move || context_open.get().then_some("dialog") aria-modal=move || context_open.get().then_some("true")
                        aria-labelledby="agent-draft-diagnostics-title" aria-describedby="agent-draft-diagnostics-help">
                        <button class="agent-draft-context-close" type="button" on:click=move |_| close_context(true)>"Close diagnostics"</button>
                        <h2 id="agent-draft-diagnostics-title">"Diagnostics and effective values"</h2>
                        <p id="agent-draft-diagnostics-help">"Select a diagnostic to move directly to its relevant control. Diagnostics are local server feedback; they do not send source content to analytics."</p>
                        <p>{validation}{move || draft.with(|value| value.as_ref().and_then(|value| value.validated_at.clone()))
                            .map(|at| format!(" at {}", String::from(js_sys::Date::new(&at.into()).to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED))))}</p>
                        <p><strong>"Change summary:"</strong>" "{move || review.with(|value| match value {
                            Some(value) if value.changed_sections.is_empty() => "No sections differ from the latest immutable version.".to_string(),
                            Some(value) => value.changed_sections.join(", "),
                            None => "Prepare review to see server-derived changed sections.".to_string(),
                        })}</p>
                        <p>"Unsaved source is never persisted in recovery storage."</p>
                        {move || {
                            let all = diagnostics.get();
                            if all.is_empty() { return view! { <p role="status">"No validation diagnostics are available."</p> }.into_any(); }
                            view! { <ul>{all.into_iter().map(|item| {
                                let name = format!("{}: {}", item.severity, item.code);
                                let message = item.message.clone();
                                view! { <li><button type="button" on:click=move |_| focus_diagnostic(&item)>{name}</button><p>{message}</p></li> }
                            }).collect_view()}</ul> }.into_any()
                        }}
                    </aside>
                </div>
                {move || guard.blocked().then(|| { let (keep, leave) = (guard, guard); let navigate = leave_navigate.clone(); view! {
                    <ConfirmationDialog title="Leave unsaved draft?" on_close=Callback::new(move |()| guard.reset())>
                        <p>"Your unsaved draft text stays in this active editor until you choose to discard it. Leave only if you are ready to lose those local changes."</p>
                        <div class="agent-draft-navigation-actions">
                            <button type="button" on:click=move |_| keep.reset()>"Keep editing"</button>
                            <button type="button" on:click=move |_| leave.proceed(&navigate)>"Discard changes and leave"</button>
                        </div>
                    </ConfirmationDialog>
                } })}
            </main>
        }
    };

    move || {
        match screen.get() {
        Screen::Loading => view! { <main class="directory agent-draft-editor" aria-label="Loading agent draft editor"><section class="agent-draft-skeleton"><div></div><div></div><div></div></section></main> }.into_any(),
        Screen::SessionError => view! { <main class="directory agent-draft-editor"><p role="alert">"Your session has expired. Sign in again to edit this draft."</p></main> }.into_any(),
        Screen::Unavailable => view! { <main class="directory agent-draft-editor"><p role="status">"This agent is unavailable."</p></main> }.into_any(),
        Screen::Error => view! {
            <main class="directory agent-draft-editor"><section class="agent-draft-error"><p role="alert">"We could not load this agent draft. Try again."</p>
                <button type="button" on:click=move |_| reload_attempt.update(|value| *value += 1)>"Retry"</button></section></main>
        }.into_any(),
        Screen::Loaded if draft.with_untracked(|value| value.as_ref().is_some_and(|value| !value.can_update)) => {
            let value = draft.get_untracked().expect("a loaded screen holds a draft");
            view! {
                <main class="directory agent-draft-editor"><h1>{value.display_name}</h1>
                    <p>"ID "{value.slug}" · "<span>"Draft revision "{value.revision}</span>" · Latest version "{value.latest_version.map_or("None".to_string(), |number| number.to_string())}</p>
                    <p role="status">"You do not have permission to edit this draft."</p></main>
            }.into_any()
        }
        Screen::Loaded => editor.clone()().into_any(),
    }
    }
}

fn document_active_element() -> Option<web_sys::Element> {
    leptos::prelude::document().active_element()
}

#[component]
fn ReviewPanel(
    review: RwSignal<Option<AgentDraftReviewFields>>,
    reviewing: RwSignal<bool>,
    warnings_acknowledged: RwSignal<bool>,
    can_publish: Signal<bool>,
    on_load: Callback<()>,
    on_publish: Callback<()>,
) -> impl IntoView {
    move || {
        let Some(value) = review.get() else {
            return view! {
                <section><p>"Review is computed by the server from the saved revision; it does not publish or deploy this agent."</p>
                    <button type="button" on:click=move |_| on_load.run(()) disabled=move || reviewing.get()>
                        {move || if reviewing.get() { "Preparing review…" } else { "Prepare review" }}</button></section>
            }.into_any();
        };
        let warnings = value
            .diagnostics
            .iter()
            .filter(|item| item.severity == "WARNING")
            .count();
        let errors = value
            .diagnostics
            .iter()
            .filter(|item| item.severity == "ERROR")
            .count();
        let (blocked, warned) = (errors > 0, warnings > 0);
        let joined = |values: &[String]| {
            if values.is_empty() {
                "None".to_string()
            } else {
                values.join(", ")
            }
        };
        view! {
            <section class="agent-draft-review">
                <p><strong>"Canonical digest:"</strong>" "<code>{value.content_digest.clone()}</code></p>
                <p><strong>"Exact catalog release:"</strong>" "{value.catalog_release_id.clone()}" ("<code>{value.catalog_release_digest.clone()}</code>")"</p>
                <p><strong>"Changed sections:"</strong>" "{joined(&value.changed_sections)}</p>
                <p><strong>"Exact dependencies:"</strong>" "{joined(&value.dependencies)}</p>
                <p><strong>"Effective configuration:"</strong>" server canonicalized from this saved document."</p>
                {blocked.then(|| view! { <p role="alert">"Publication is blocked by "{errors}" validation error(s)."</p> })}
                {warned.then(|| view! { <label><input type="checkbox" prop:checked=move || warnings_acknowledged.get()
                    on:change=move |event| warnings_acknowledged.set(event_target_checked(&event)) />" I reviewed the "{warnings}" warning(s)."</label> })}
                <button type="button" on:click=move |_| on_publish.run(())
                    disabled=move || !can_publish.get() || blocked || (warned && !warnings_acknowledged.get())>"Publish immutable version"</button>
            </section>
        }.into_any()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_matches_the_react_status_rendering() {
        assert_eq!(title("VALID"), "Valid");
        assert_eq!(title("NOT_VALIDATED"), "Not_Validated");
    }
}
