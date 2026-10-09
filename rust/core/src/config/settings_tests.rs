use super::default_command;

#[test]
fn unknown_command_default_is_empty() {
    assert_eq!(default_command("unknown command"), "");
}
