use nectra::{ollama, session::Session};
use std::io::Cursor;

#[test]
fn ollama_preserves_roles_from_the_actual_shared_session() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(&root.path().join("session.jsonl"), root.path()).unwrap();
    session.begin("Remember cobalt", "test-model").unwrap();
    session.complete("Remembered").unwrap();
    let (prompt, _) = session.prompt("What color?", 128 * 1024).unwrap();
    let messages = ollama::messages(&prompt).unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "Remember cobalt");
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["content"], "Remembered");
    assert_eq!(messages[3]["content"], "What color?");
}

#[test]
fn ollama_does_not_mark_a_length_limited_answer_complete() {
    let event = "{\"message\":{\"content\":\"unfinished answer\"},\"done\":true,\"done_reason\":\"length\"}\n";
    let mut output = Vec::new();
    let error = ollama::stream(Cursor::new(event), &mut output).unwrap_err();
    assert!(error.to_string().contains("output limit"));
    assert_eq!(String::from_utf8(output).unwrap(), "unfinished answer");
    assert!(
        error.downcast_ref::<ollama::Unavailable>().is_none(),
        "partial answers must not trigger failover"
    );
}
