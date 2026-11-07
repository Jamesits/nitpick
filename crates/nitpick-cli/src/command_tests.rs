use super::*;

#[test]
fn parses_each_command() {
    let cases = [
        ("help", Command::Help),
        ("displays", Command::Displays),
        (
            "select browser:client-1",
            Command::Select("browser:client-1".into()),
        ),
        (
            "pattern pq-grayscale",
            Command::Pattern(PatternKind::Grayscale),
        ),
        (
            "format hlg-narrow 12",
            Command::Format(Encoding::HlgNarrow, BitDepth::Twelve),
        ),
        ("  show  ", Command::Show),
        ("table", Command::Table),
        ("close", Command::Close),
        ("quit", Command::Quit),
    ];
    for (line, command) in cases {
        assert_eq!(parse(line), Ok(Some(command)), "{line}");
    }
}

#[test]
fn empty_line_is_no_command() {
    assert_eq!(parse(""), Ok(None));
    assert_eq!(parse(" \t "), Ok(None));
}

#[test]
fn rejects_unknown_commands_and_arguments() {
    for line in [
        "exit",
        "show now",
        "select",
        "select a b",
        "pattern",
        "pattern smpte",
        "format pq-narrow",
        "format pq-narrow 8",
        "format sdr 10",
    ] {
        assert!(parse(line).is_err(), "{line}");
    }
}
