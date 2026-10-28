// SPDX-License-Identifier: Apache-2.0
use super::*;

fn size(s: &str) -> Result<u64, String> {
    s.parse::<Size>().map(Size::bytes).map_err(|e| e.0)
}

#[test]
fn sizes_parse_bytes_hex_and_binary_suffixes() {
    for (text, want) in [("24", 24), ("0x40", 64), ("4K", 4096), ("64M", 64 << 20), ("1G", 1 << 30), ("0", 0)] {
        assert_eq!(size(text), Ok(want), "{text}");
    }
    for bad in ["4k", "1.5M", "-1", "", "0x", "0X40", "K", "0x1K", "4 K", "0x11112222333344445"] {
        let e = size(bad).unwrap_err();
        assert!(e.contains("use bytes (24), 0x hex (0x40), or a K, M, G suffix"), "{bad}: {e}");
    }
    assert_eq!(size("18446744073709551616"), Err("size \"18446744073709551616\" does not fit 64 bits".into()));
    assert_eq!(size("17179869184G"), Err("size \"17179869184G\" does not fit 64 bits".into()));
}

#[test]
fn a_size_prints_with_the_largest_suffix_that_divides_it_and_parses_back() {
    for (n, text) in [(0, "0"), (24, "24"), (1024, "1K"), (1536, "1536"), (3 << 20, "3M"), (2411724, "2411724")] {
        assert_eq!(Size(n).to_string(), text);
    }
    assert_eq!(Size(5 << 30).to_string(), "5G");
    for n in [0, 1, 24, 1023, 1024, 1536, 4096, 1 << 20, 3 << 20, 1 << 30, 5 << 30, 1 << 63, u64::MAX, 2411724] {
        assert_eq!(Size(n).to_string().parse::<Size>(), Ok(Size(n)), "{n}");
    }
}

#[test]
fn durations_parse_milliseconds_or_hours_minutes_and_seconds_in_order() {
    for (text, ms) in [
        ("500ms", 500),
        ("0ms", 0),
        ("30s", 30_000),
        ("41.3s", 41_300),
        ("0.5s", 500),
        ("1.25s", 1_250),
        ("5m", 300_000),
        ("2m40s", 160_000),
        ("1h30m", 5_400_000),
        ("1h", 3_600_000),
        ("1h1s", 3_601_000),
        ("1m30.5s", 90_500),
        ("90s", 90_000),
    ] {
        assert_eq!(parse_ms(text), Ok(ms), "{text}");
    }
    for bad in
        ["", "5", "5x", "1.5m", "1.2345s", "-1s", "1s1m", "1m1m", " 1s", "1.s", ".5s", "s", "m", "1h 2m", "1ms5", "+1s"]
    {
        let e = parse_ms(bad).unwrap_err().0;
        assert!(e.ends_with("use 500ms, 30s, 41.3s, 5m, 2m40s or 1h30m"), "{bad}: {e}");
    }
    assert_eq!(parse_ms("99999999999999999999s").unwrap_err().0, "duration \"99999999999999999999s\" is too long");
}

#[test]
fn a_duration_prints_at_its_precision_rounded_half_up() {
    for (ms, text) in [
        (0, "0ms"),
        (840, "840ms"),
        (999, "999ms"),
        (1_000, "1s"),
        (41_000, "41s"),
        (41_300, "41.3s"),
        (41_350, "41.4s"),
        (41_349, "41.3s"),
        (59_949, "59.9s"),
        (59_950, "1m"),
        (60_000, "1m"),
        (89_500, "1m30s"),
        (160_000, "2m40s"),
        (2_700_000, "45m"),
        (3_601_000, "1h1s"),
        (7_200_000, "2h"),
    ] {
        assert_eq!(display_ms(ms), text, "{ms}");
    }
    for ms in [0, 840, 999, 1_000, 1_100, 41_300, 59_900, 60_000, 61_000, 160_000, 2_700_000, 3_601_000, 7_200_000] {
        assert_eq!(parse_ms(&display_ms(ms)), Ok(ms), "{ms}");
    }
}

#[test]
fn a_duration_outside_its_type_is_refused_not_clamped() {
    assert_eq!("2h".parse::<ScriptTimeout>().map(Dur::as_ms), Ok(7_200_000));
    assert_eq!("1s".parse::<ScriptTimeout>().map(|d| d.to_string()), Ok("1s".into()));
    assert_eq!("2h1s".parse::<ScriptTimeout>().unwrap_err().0, "duration 2h1s is outside 1s..2h");
    assert_eq!("500ms".parse::<ScriptTimeout>().unwrap_err().0, "duration 500ms is outside 1s..2h");
    assert_eq!("61m".parse::<TransferTimeout>().unwrap_err().0, "duration 1h1m is outside 1s..1h");
    assert_eq!(TransferTimeout::from_ms(600_000).map(Dur::as_duration), Ok(Duration::from_secs(600)));
}

fn place(s: &str) -> Result<Place, String> {
    s.parse::<Place>().map_err(|e| e.0)
}

fn class(c: &str) -> Option<AccessClass> {
    Some(AccessClass::parse(c).unwrap())
}

#[test]
fn places_are_class_addresses_or_register_relative() {
    let reg = |r: &str, offset| Place::Reg { reg: RegName::new(r).unwrap(), offset };
    for (text, want, shown) in [
        ("0x40107e40", Place::Addr { class: None, addr: 0x4010_7e40 }, "0x40107e40"),
        ("A:0x40000000", Place::Addr { class: class("A"), addr: 0x4000_0000 }, "A:0x40000000"),
        ("d:0x4000ABCD", Place::Addr { class: class("D"), addr: 0x4000_abcd }, "D:0x4000abcd"),
        ("0xffffffffffffffff", Place::Addr { class: None, addr: u64::MAX }, "0xffffffffffffffff"),
        ("@sp", reg("sp", 0), "@sp"),
        ("@sp+0x0", reg("sp", 0), "@sp"),
        ("@x1+0x10", reg("x1", 16), "@x1+0x10"),
        ("@x1-0x8", reg("x1", -8), "@x1-0x8"),
    ] {
        let got = place(text).unwrap();
        assert_eq!(got, want, "{text}");
        assert_eq!(got.to_string(), shown, "{text}");
        assert_eq!(place(&got.to_string()), Ok(want), "{text} parses back");
    }
}

#[test]
fn planned_and_malformed_places_name_the_accepted_forms() {
    let forms = "use [CLASS:]0x<hex> or @<reg>[+0x<hex>|-0x<hex>]";
    for (bad, why) in [
        ("40000000", "a bare number is never an address"),
        ("main", "symbol places are planned, not in this t32ctl"),
        ("worker_loop+0x84", "symbol places are planned, not in this t32ctl"),
        ("A:main", "symbol places are planned, not in this t32ctl"),
        ("src/worker.c:61", "source-line places are planned, not in this t32ctl"),
        ("worker.c:61", "source-line places are planned, not in this t32ctl"),
        ("Z!:0x10", "\"Z!\" is not an access class"),
        ("0x", ""),
        ("0x1g", ""),
        ("0x11112222333344445", ""),
        ("", ""),
    ] {
        let e = place(bad).unwrap_err();
        assert!(e.starts_with(&format!("place {bad:?}: {why}")), "{bad}: {e}");
        assert!(e.ends_with(forms), "{bad}: {e}");
    }
    for (bad, want) in [
        ("@", "register name \"\" is not"),
        ("@1x", "register name \"1x\" is not"),
        ("@sp+16", "a register offset is +0x<hex> or -0x<hex>"),
        ("@sp+", "a register offset is +0x<hex> or -0x<hex>"),
        ("@sp+0x10+0x4", "a register offset is +0x<hex> or -0x<hex>"),
        ("@sp-0x8000000000000000", "a register offset is +0x<hex> or -0x<hex>"),
    ] {
        let e = place(bad).unwrap_err();
        assert!(e.starts_with(&format!("place {bad:?}: {want}")), "{bad}: {e}");
    }
}

#[test]
fn output_names_are_plain_file_names() {
    for ok in ["dump.bin", "ddr.bin", "a-b_c.txt", "x"] {
        assert_eq!(ok.parse::<OutName>().map(|n| n.to_string()), Ok(ok.to_string()));
    }
    for bad in ["a/b", ".hidden", "", "con", "x ", "x\\y", "..", "a&b"] {
        let e = bad.parse::<OutName>().unwrap_err().0;
        assert!(e.starts_with(&format!("output name {bad:?}: ")), "{bad}: {e}");
    }
}

#[test]
fn practice_text_is_one_line_of_at_most_2037_bytes() {
    for ok in ["OS.PTD()", "&t32ctl_x", "PRINT \"a b\"", &"x".repeat(2037)] {
        assert_eq!(ok.parse::<PracticeText>().map(|t| t.to_string()), Ok(ok.to_string()));
    }
    assert_eq!(
        "x".repeat(2038).parse::<PracticeText>().unwrap_err().0,
        "PRACTICE text is 2038 bytes; it must be 1 to 2037 bytes"
    );
    assert_eq!("".parse::<PracticeText>().unwrap_err().0, "PRACTICE text is 0 bytes; it must be 1 to 2037 bytes");
    for bad in ["a\nb", "a\tb", "a\rb", "a\u{7f}"] {
        let e = bad.parse::<PracticeText>().unwrap_err().0;
        assert!(e.starts_with("PRACTICE text contains the control character"), "{bad:?}: {e}");
    }
}

#[test]
fn integers_are_decimal_or_hex_and_print_in_decimal() {
    for (text, v) in [("42", 42), ("0x2a", 42), ("-1", -1), ("-0", 0), ("007", 7), ("0x7fffffffffffffff", i64::MAX)] {
        let got = text.parse::<IntValue>().unwrap();
        assert_eq!(got, IntValue(v), "{text}");
        assert_eq!(got.to_string().parse::<IntValue>(), Ok(got), "{text}");
    }
    assert_eq!(IntValue(42).to_string(), "42");
    for bad in ["", "-", "4 2", "0x", "+1", "-0x2a", "1.5", "0x8000000000000000", "9223372036854775808"] {
        let e = bad.parse::<IntValue>().unwrap_err().0;
        assert_eq!(e, format!("integer {bad:?}: use 42, 0x2a or -1, within 64 bits"));
    }
}

#[test]
fn shell_words_are_quoted_only_when_needed() {
    for (s, want) in [
        ("t32ctl", "t32ctl"),
        ("D:0x10", "D:0x10"),
        ("--part=user", "--part=user"),
        ("100%", "100%"),
        ("@x1+0x10", "@x1+0x10"),
        ("a b", "'a b'"),
        ("", "''"),
        ("it's", "'it'\\''s'"),
        ("{build}/x", "'{build}/x'"),
    ] {
        assert_eq!(shell_word(s), want, "{s}");
    }
}
