use clap::Parser;

use super::Args;

fn delay(args: &[&str]) -> Result<u32, clap::Error> {
    Args::try_parse_from(["nitpick-cli"].iter().chain(args)).map(|a| a.delay)
}

#[test]
fn delay_option_defaults_to_the_shared_value() {
    assert_eq!(delay(&[]).unwrap(), 1000);
    assert_eq!(delay(&["--delay", "0"]).unwrap(), 0);
    assert_eq!(delay(&["--delay=2500"]).unwrap(), 2500);
}

#[test]
fn delay_option_rejects_invalid_values() {
    for value in ["-1", "1.5", "4294967296", "soon", ""] {
        let e = delay(&[&format!("--delay={value}")]).unwrap_err();
        assert_eq!(e.kind(), clap::error::ErrorKind::ValueValidation, "{value}");
    }
}
