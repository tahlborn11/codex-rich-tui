use crate::markdown_render::render_markdown_text_with_width;
use insta::assert_snapshot;

fn markdown_text(source: &str, width: usize) -> String {
    render_markdown_text_with_width(source, Some(width))
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn mermaid_fences_use_native_renderer_for_every_family() {
    for source in [
        "%% heading\nflowchart TD; A --> B",
        "graph LR; A --> B",
        "flowchart; A -- send --> B & C; B <-.-> C",
        "sequenceDiagram; A->>B: request; B-->>A: response",
        "stateDiagram-v2; [*] --> Active; Active --> [*]",
        "stateDiagram; [*] --> Active; Active --> [*]",
        "classDiagram; Order \"1\" *-- \"many\" Item : contains",
        "erDiagram; CUSTOMER ||--o{ ORDER : places",
    ] {
        let markdown = format!("```mermaid title=example\n{source}\n```\n");
        let rendered = markdown_text(&markdown, /*width*/ 100);
        assert!(rendered.starts_with("╭─ mermaid"));
        for line in codex_mermaid::render(source, /*max_width*/ 100)
            .unwrap()
            .lines()
        {
            assert!(rendered.contains(line));
        }
    }
}

#[test]
fn mermaid_nested_fences_and_unicode() {
    let source = "> ~~~~mermaid\n> flowchart TD\n>     A[请求] --> B[Réponse]\n> ~~~~~\n\n- Diagram:\n\n  ```mermaid\n  flowchart LR\n      A --> B\n  ```\n";
    assert_snapshot!(markdown_text(source, /*width*/ 60));
}

#[test]
fn mermaid_quoted_labels_and_ampersands() {
    let source = r#"```mermaid
flowchart LR
    A["Your saved order"] --> B["Review & confirm"] --> C["DoorDash checkout"]
```"#;
    let output = markdown_text(source, /*width*/ 100);
    assert!(output.starts_with("╭─ mermaid"));
    assert!(output.contains('┌'));
    assert_snapshot!(output);
}

#[test]
fn mermaid_entities_keep_source() {
    let source = "```mermaid\nsequenceDiagram\nA->>B: &amp;\n```";
    let output = markdown_text(source, /*width*/ 100);
    assert!(output.starts_with("╭─ mermaid"));
    assert!(output.contains("A->>B: &amp;"));
}

#[test]
fn mermaid_stadium_flowchart() {
    let source = "```mermaid
flowchart TD
    A([What should I work on?]) --> B{Anything urgent?}
    B -->|Yes| C[Handle the urgent task]
    B -->|No| D{Have a clear goal?}
    D -->|No| E[Pick one useful outcome]
    E --> F[Choose the smallest next step]
    D -->|Yes| F
    F --> G[Focus for 25 minutes]
    C --> H{Done?}
    G --> H
    H -->|No| I[Take a short break]
    I --> F
    H -->|Yes| J([Celebrate. Stretch. Repeat.])
```";
    let output = markdown_text(source, /*width*/ 100);
    assert!(output.starts_with('╭'));
    assert_snapshot!(output);
    let narrow = markdown_text(source, /*width*/ 40);
    assert!(narrow.starts_with("╭─ mermaid"));
    assert!(narrow.contains("flowchart TD"));
}

#[test]
fn mermaid_unclosed_blocks_keep_source_without_notice() {
    for (source, width) in [
        ("```mermaid\nflowchart LR\nA --> B\n", 80),
        ("```mermaid\nflowchart TD\nA[unfinished\n", 80),
        ("````mermaid\nflowchart LR\nA --> B\n```\n", 80),
        ("> ```mermaid\n> flowchart LR\n> A --> B\n", 80),
    ] {
        let rendered = markdown_text(source, width);
        assert!(!rendered.is_empty(), "source: {source:?}");
        assert!(rendered.contains("flowchart"), "source: {source:?}");
    }
}

#[test]
fn mermaid_fallback_notices_preserve_source() {
    let mut cases = Vec::new();
    for (name, source, width) in [
        ("invalid", "```mermaid\nflowchart LR\nA[unfinished\n```", 80),
        ("unsupported", "```mermaid\npie\n\"Cats\": 2\n```", 80),
        (
            "unsupported shape after supported edges",
            "```mermaid\nflowchart TD\nP --> Q\nA[(Database)]\n```",
            80,
        ),
        (
            "Markdown string after supported edges",
            "```mermaid\nflowchart TD\nP --> Q\nA[\"`hello **world**`\"]\n```",
            80,
        ),
        (
            "too wide",
            "```mermaid\nflowchart LR\nA[Request] --> B[Reply]\n```",
            8,
        ),
        (
            "limit",
            "```mermaid\nflowchart TD\nA[This label exceeds the forty column limit]\n```",
            80,
        ),
    ] {
        let rendered = render_markdown_text_with_width(source, Some(width));
        let notice = rendered
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains("This"))
            .expect("fallback notice");
        assert!(
            notice
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::DIM),
            "{rendered:?}"
        );
        let output = rendered.to_string();
        let normalized_output = output
            .chars()
            .filter(|character| character.is_alphanumeric())
            .collect::<String>();
        for line in source
            .lines()
            .skip(1)
            .filter(|line| !line.trim_start().starts_with("```") && !line.trim().is_empty())
        {
            let normalized_line = line
                .chars()
                .filter(|character| character.is_alphanumeric())
                .collect::<String>();
            assert!(
                normalized_output.contains(&normalized_line),
                "source line: {line:?}"
            );
        }
        cases.push(format!("{name}\n{output}"));
    }
    assert_snapshot!(cases.join("\n\n---\n\n"));
}

#[test]
fn mermaid_fallback_notices_follow_nested_indentation() {
    let source = "> ```mermaid\n> pie\n> \"Cats\": 2\n> ```\n\n- Diagram:\n\n  ```mermaid\n  pie\n  \"Cats\": 2\n  ```\n";
    assert_snapshot!(markdown_text(source, /*width*/ 40));
}

#[test]
fn mermaid_styles_follow_the_supplied_theme() {
    use two_face::theme::EmbeddedThemeName;

    let themes = two_face::theme::extra();
    let fallback = syntect::highlighting::Theme::default();
    let mut cases = Vec::new();
    for (name, theme) in [
        ("dark", themes.get(EmbeddedThemeName::Dracula)),
        ("light", themes.get(EmbeddedThemeName::SolarizedLight)),
        ("fallback", &fallback),
    ] {
        let colors = if name == "light" {
            crate::terminal_probe::DefaultColors {
                fg: (30, 30, 30),
                bg: (255, 255, 255),
            }
        } else {
            crate::terminal_probe::DefaultColors {
                fg: (220, 220, 220),
                bg: (20, 20, 20),
            }
        };
        let lines = crate::terminal_palette::with_test_default_colors(colors, || {
            super::render(
                "flowchart LR; A[请求] --> B[Reply]",
                /*width*/ Some(40),
                theme,
            )
        });
        let styled = lines
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| format!("{:?} {:?}", span.style, span.content))
                    .collect::<Vec<_>>()
                    .join(" | ")
            })
            .collect::<Vec<_>>()
            .join("\n");
        cases.push(format!("{name}\n{styled}"));
    }
    assert_snapshot!(cases.join("\n\n"));
}
