//! Eine einzelne Session als Markdown — für den Session-Branch (Track C).
//!
//! GitLab (und GitHub) rendern eine `session.md` im Branch **nativ**. Damit wird
//! der Branch selbst zur lesbaren Session-Seite, ohne dass irgendein Reader
//! deployt werden muss — genau der „mehr ins Repo"-Zug. Rein und deterministisch:
//! gleiche Session ⇒ byte-gleiches Markdown, 0 Tokens.
//!
//! # Warum hier und nicht im Reader
//!
//! `minds-store` schreibt die `session.md` in den Branch und darf **nicht** von
//! `minds-reader` abhängen — der Reader hängt am Store, das wäre ein Zyklus. Also
//! lebt der Renderer hier, wo Store und Reader ihn beide erreichen. Kein I/O, wie
//! der Rest von `minds-core`.
//!
//! # Kein HTML-Escaping nötig
//!
//! Die Ausgabe ist Markdown, kein HTML, und die Session ist bereits redigiert
//! (Secrets/PII raus, bevor sie in den Store ging). GitLabs Markdown-Renderer
//! sanitisiert selbst; ein Prompt mit `#`-Zeichen sieht höchstens ungewohnt aus,
//! ist aber keine Lücke.

use std::fmt::Write as _;

use crate::{EffectKind, Role, Session, SessionId, ToolCall};

/// Rendert eine Session als Markdown-Dokument.
pub fn session_markdown(id: SessionId, session: &Session) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {}\n", headline(&session.intent.request));

    let _ = writeln!(
        out,
        "**Agent:** {} {} · **Modell:** {}/{}  \n\
         **Tokens:** {} ein / {} aus · **Session:** `{}`\n",
        session.agent.name,
        session.agent.version,
        session.model.provider,
        session.model.id,
        session.usage.input_tokens,
        session.usage.output_tokens,
        id,
    );

    let _ = writeln!(out, "## Absicht\n");
    let request = session.intent.request.trim();
    if request.is_empty() {
        let _ = writeln!(out, "_(kein Prompt erfasst)_\n");
    } else {
        let _ = writeln!(out, "{request}\n");
    }

    list_section(&mut out, "Constraints", &session.intent.constraints, 3);
    list_section(&mut out, "Verworfene Ansätze", &session.intent.discarded, 3);

    if !session.turns.is_empty() {
        let _ = writeln!(out, "## Verlauf\n");
        for turn in &session.turns {
            let _ = writeln!(out, "**{}**\n", role_label(&turn.role));
            let text = turn.text.trim();
            if !text.is_empty() {
                let _ = writeln!(out, "{text}\n");
            }
            for call in &turn.tool_calls {
                let _ = writeln!(out, "- {}", tool_line(call));
            }
            if !turn.tool_calls.is_empty() {
                out.push('\n');
            }
        }
    }

    if !session.produced.files.is_empty() {
        let _ = writeln!(out, "## Berührte Dateien\n");
        for file in &session.produced.files {
            let _ = writeln!(out, "- `{file}`");
        }
        out.push('\n');
    }

    if session.redaction.applied {
        let counts = &session.redaction.counts;
        let _ = writeln!(
            out,
            "## Redaction\n\n{} Secret(s), {} PII entfernt.",
            counts.secrets, counts.pii
        );
    }

    out
}

/// Ein `##`-Abschnitt mit Titel `###` und Liste — nur, wenn er Einträge hat.
fn list_section(out: &mut String, title: &str, items: &[String], _level: usize) {
    if items.is_empty() {
        return;
    }
    let _ = writeln!(out, "### {title}\n");
    for item in items {
        let _ = writeln!(out, "- {item}");
    }
    out.push('\n');
}

/// Eine Tool-Call-Zeile: Name, Effekt und das Wesentliche (Kommando bei Exec,
/// sonst der Pfad).
fn tool_line(call: &ToolCall) -> String {
    let effect = call
        .effect
        .as_ref()
        .map(|e| effect_label(e.kind))
        .unwrap_or("tool");
    let detail = call
        .effect
        .as_ref()
        .and_then(|e| {
            if e.kind == EffectKind::Exec {
                crate::extract::command_of(&call.arguments)
            } else {
                e.path.clone()
            }
        })
        .unwrap_or_default();
    if detail.is_empty() {
        format!("`{}` ({effect})", call.name)
    } else {
        format!("`{}` ({effect}) `{detail}`", call.name)
    }
}

fn role_label(role: &Role) -> &'static str {
    match role {
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::System => "System",
        Role::Tool => "Tool",
    }
}

fn effect_label(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::Read => "read",
        EffectKind::Write => "write",
        EffectKind::Delete => "delete",
        EffectKind::Exec => "exec",
        EffectKind::Other => "tool",
    }
}

/// Die erste nicht-leere Zeile des Prompts, auf 80 Zeichen gekürzt; leer wird zu
/// „Session".
fn headline(request: &str) -> String {
    let line = request
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Session");
    if line.chars().count() <= 80 {
        line.to_string()
    } else {
        let mut out: String = line.chars().take(79).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Agent, Effect, Intent, Model, Produced, Redaction, RedactionCounts, ToolCall, Turn, Usage,
    };

    fn sid() -> SessionId {
        format!("b3-{}", "a".repeat(64)).parse().unwrap()
    }

    fn sample() -> Session {
        let mut s = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1.4.2".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4".into(),
            },
            Intent {
                request: "Retry-Test reparieren".into(),
                constraints: vec!["keine neuen Dependencies".into()],
                discarded: vec!["scratch.rs — angelegt und wieder entfernt".into()],
            },
        );
        s.usage = Usage {
            input_tokens: 900,
            output_tokens: 120,
        };
        s.produced = Produced {
            commit_hint: None,
            files: vec!["src/retry.rs".into()],
        };
        s.redaction = Redaction {
            applied: true,
            counts: RedactionCounts { secrets: 1, pii: 2 },
        };
        s.turns.push(Turn {
            role: Role::User,
            text: "Der Retry-Test flackert.".into(),
            tool_calls: Vec::new(),
            parent: None,
            at: None,
        });
        s.turns.push(Turn {
            role: Role::Assistant,
            text: "Ich sehe mir die Backoff-Logik an.".into(),
            tool_calls: vec![ToolCall {
                capture: None,
                name: "Bash".into(),
                arguments: r#"{"command":"cargo test retry"}"#.into(),
                effect: Some(Effect {
                    kind: EffectKind::Exec,
                    path: None,
                    content: None,
                }),
            }],
            parent: None,
            at: None,
        });
        s
    }

    #[test]
    fn markdown_has_all_the_sections() {
        let md = session_markdown(sid(), &sample());
        assert!(md.starts_with("# Retry-Test reparieren"));
        assert!(md.contains("**Agent:** claude-code 1.4.2"));
        assert!(md.contains("anthropic/claude-opus-4"));
        assert!(md.contains("900 ein / 120 aus"));
        assert!(md.contains("## Absicht"));
        assert!(md.contains("### Constraints"));
        assert!(md.contains("keine neuen Dependencies"));
        assert!(md.contains("### Verworfene Ansätze"));
        assert!(md.contains("## Verlauf"));
        assert!(md.contains("**User**"));
        assert!(md.contains("**Assistant**"));
        // Exec-Kommando entrauscht.
        assert!(md.contains("`Bash` (exec) `cargo test retry`"));
        assert!(md.contains("## Berührte Dateien"));
        assert!(md.contains("`src/retry.rs`"));
        assert!(md.contains("## Redaction\n\n1 Secret(s), 2 PII entfernt."));
    }

    #[test]
    fn the_same_session_renders_identically() {
        assert_eq!(
            session_markdown(sid(), &sample()),
            session_markdown(sid(), &sample())
        );
    }

    #[test]
    fn an_empty_prompt_says_so() {
        let s = Session::new(
            Agent {
                name: "a".into(),
                version: "1".into(),
            },
            Model {
                provider: "p".into(),
                id: "m".into(),
            },
            Intent::default(),
        );
        let md = session_markdown(sid(), &s);
        assert!(md.starts_with("# Session"));
        assert!(md.contains("_(kein Prompt erfasst)_"));
    }
}
