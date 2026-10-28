// SPDX-License-Identifier: Apache-2.0
use super::*;

fn tokens(line: &str) -> Vec<String> {
    line.split(' ').filter(|t| !t.is_empty()).map(String::from).collect()
}

#[test]
fn the_global_flags_come_out_of_the_tokens_and_the_rest_are_pairs() {
    let call = split(&tokens("--part=user --json --offset 0x10 --build=out/b --help --label --x"));
    assert_eq!(
        call,
        ProcCall {
            params: vec![
                ("--part".into(), "user".into()),
                ("--offset".into(), "0x10".into()),
                ("--label".into(), "--x".into()),
            ],
            json: true,
            build: Some("out/b".into()),
            help: true,
            stray: vec![],
        }
    );
    let call = split(&tokens("x --build b --offset"));
    assert_eq!(call.build, Some("b".into()));
    assert_eq!(call.stray, ["x is not a --PARAM VALUE pair", "--offset needs a value"]);
    assert_eq!(split(&tokens("--build")).stray, ["--build needs DIR"]);
}
