use pretty_assertions::assert_eq;

use super::last_fenced_code_block;

#[test]
fn returns_the_last_non_empty_fenced_block_verbatim() {
    let markdown = "Before\n```rust\nfn first() {}\n```\n\n    indented\n\n```json\n{\"ok\": true}\n```\n```text\n```\n";

    assert_eq!(
        last_fenced_code_block(markdown),
        Some("{\"ok\": true}\n".to_string())
    );
}

#[test]
fn returns_none_without_a_non_empty_fenced_block() {
    assert_eq!(
        last_fenced_code_block("Text\n\n    indented code\n\n```text\n \n\t\n```\n"),
        None
    );
}
