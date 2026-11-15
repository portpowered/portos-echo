use std::process::ExitCode;

fn dispatch(subcommand: Option<&str>) -> Result<(), &'static str> {
    match subcommand {
        _ => Err("Usage: cargo xtask <subcommand>"),
    }
}

fn main() -> ExitCode {
    let subcommand = std::env::args().nth(1);
    match dispatch(subcommand.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::dispatch;

    #[test]
    fn missing_subcommand_returns_usage() {
        assert_eq!(dispatch(None), Err("Usage: cargo xtask <subcommand>"));
    }

    #[test]
    fn unknown_subcommand_returns_usage() {
        assert_eq!(
            dispatch(Some("unknown")),
            Err("Usage: cargo xtask <subcommand>")
        );
    }
}
