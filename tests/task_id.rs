use second::{TaskId, TaskIdParseError};

#[test]
fn task_id_preserves_the_exact_valid_identifier() {
    let text = "Az09_-request";
    let task_id = TaskId::parse(text).unwrap();

    assert_eq!(task_id.as_bytes(), text.as_bytes());
    assert_eq!(task_id.to_string(), text);
}

#[test]
fn task_id_enforces_the_full_grammar_and_byte_bounds() {
    let max = "A".repeat(128);
    assert!(TaskId::parse(&max).is_ok());

    assert_eq!(TaskId::parse(""), Err(TaskIdParseError::Empty));
    assert_eq!(
        TaskId::parse(&"A".repeat(129)),
        Err(TaskIdParseError::TooLong)
    );

    for invalid in ["has space", "slash/value", "é", "dot.value"] {
        assert_eq!(
            TaskId::parse(invalid),
            Err(TaskIdParseError::InvalidCharacter)
        );
    }
}
