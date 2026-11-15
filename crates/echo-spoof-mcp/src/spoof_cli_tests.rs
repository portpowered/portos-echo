use super::*;

#[test]
fn parses_exact_cli_and_refuses_invalid_flags() {
    assert!(parse(&["--help".into()]).unwrap().is_none());
    let options = parse(&[
        "--port".into(),
        "23".into(),
        "--fault".into(),
        "slow".into(),
        "--slow-ms".into(),
        "7".into(),
    ])
    .unwrap()
    .unwrap();
    assert_eq!((options.port, options.fault, options.slow_ms), (23, Fault::Slow, 7));
    for args in [
        vec!["--bad"],
        vec!["--port"],
        vec!["--help", "--port"],
        vec!["--port", "65536"],
        vec!["--slow-ms", "-1"],
        vec!["--fault", "bad"],
        vec!["--port", "0", "--port", "1"],
    ] {
        assert!(parse(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err());
    }
    assert_eq!(parse(&[]).unwrap().unwrap().port, 0);
}
