use super::TuiArgs;
use crate::ai::agent::conversation::AIConversationId;

/// Parses a prompt and typed conversation ID.
#[test]
fn parses_prompt_and_conversation_id() {
    let conversation_id = AIConversationId::new();
    assert_eq!(
        TuiArgs::parse([
            "--conversation-id".to_owned(),
            conversation_id.to_string(),
            "--prompt".to_owned(),
            "hello".to_owned(),
        ],)
        .unwrap(),
        TuiArgs {
            prompt: Some("hello".to_owned()),
            conversation_id: Some(conversation_id),
        }
    );
}

/// Rejects flags whose values are missing.
#[test]
fn rejects_missing_argument_value() {
    let error = TuiArgs::parse(["--prompt".to_owned()]).unwrap_err();
    assert_eq!(error.to_string(), "--prompt requires a value");
}

/// Rejects malformed conversation IDs during frontend argument parsing.
#[test]
fn rejects_invalid_conversation_id() {
    let error = TuiArgs::parse(["--conversation-id".to_owned(), "invalid".to_owned()]).unwrap_err();
    assert!(error.to_string().starts_with("Invalid conversation ID:"));
}

/// Rejects unsupported TUI frontend arguments.
#[test]
fn rejects_unknown_argument() {
    let error = TuiArgs::parse(["--unknown".to_owned()]).unwrap_err();
    assert_eq!(error.to_string(), "Unknown argument: --unknown");
}
