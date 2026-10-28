// SPDX-License-Identifier: Apache-2.0
use super::*;

/// A fresh directory under the system temp dir, removed when dropped.
struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("t32ctl-config-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
        p
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let p = self.0.join(rel);
        fs::create_dir_all(&p).unwrap();
        p
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn env_at(cwd: &Path) -> Env {
    Env {
        cwd: cwd.to_path_buf(),
        home: Some("/home/op".into()),
        auto_hosts: vec!["127.0.0.1".into()],
        ..Env::default()
    }
}

#[test]
fn files_follow_xdg_then_home_and_ignore_a_relative_xdg_dir() {
    let mut env = Env { home: Some("/home/op".into()), ..Env::default() };
    let p = Paths::new(&env).unwrap();
    assert_eq!(p.config_file, Some(PathBuf::from("/home/op/.config/t32ctl/config.toml")));
    assert_eq!(p.state_base, Path::new("/home/op/.local/state/t32ctl"));
    assert_eq!(p.out_base(), Path::new("/home/op/.local/state/t32ctl/out"));
    assert_eq!(p.pv_dir(&Config::default()), Path::new("/home/op/.local/state/t32ctl/pv/auto-20000"));
    env.xdg_config_home = Some("/x/cfg".into());
    env.xdg_state_home = Some("relative/state".into());
    let p = Paths::new(&env).unwrap();
    assert_eq!(p.config_file, Some(PathBuf::from("/x/cfg/t32ctl/config.toml")));
    assert_eq!(p.state_base, Path::new("/home/op/.local/state/t32ctl"));
    let homeless = Env { xdg_state_home: Some("/x/state".into()), ..Env::default() };
    let p = Paths::new(&homeless).unwrap();
    assert_eq!((p.config_file, p.state_base), (None, PathBuf::from("/x/state/t32ctl")));
    let e = Paths::new(&Env::default()).unwrap_err();
    assert_eq!((e.kind, e.message.as_str()), (ErrorKind::StateDir, "no state directory: set XDG_STATE_HOME or HOME"));
}

#[test]
fn paths_print_relative_below_the_current_directory_and_with_a_tilde_below_home() {
    let env = env_at(Path::new("/home/op/ws"));
    assert_eq!(env.display(Path::new("/home/op/ws/build")), "build");
    assert_eq!(env.display(Path::new("/home/op/ws")), ".");
    assert_eq!(env.display(Path::new("/home/op/.config/t32ctl/config.toml")), "~/.config/t32ctl/config.toml");
    assert_eq!(env.display(Path::new("/home/op")), "~");
    assert_eq!(env.display(Path::new("/opt/x")), "/opt/x");
}

#[test]
fn a_missing_config_file_means_the_defaults() {
    let c = Config::load(None).unwrap();
    assert_eq!(c, Config::default());
    assert_eq!(Config::load(Some(Path::new("/nonexistent-t32ctl/config.toml"))).unwrap(), c);
    assert_eq!((c.host.as_str(), c.port), ("auto", 20000));
    assert_eq!((c.window.base(), c.window.len()), (0x0100_0000, 0x0400_0000));
    assert_eq!((c.tuning.read_chunk(), c.tuning.write_chunk()), (15360, 15360));
    assert_eq!(c.tuning.macro_spelling(), t32rcl::MacroSpelling::Ampersand);
    assert_eq!((c.transfer_timeout.to_string(), c.transfer_max.to_string()), ("10m".into(), "1G".into()));
    assert_eq!(c.stage_root, None);
}

const FILE: &str = "/home/op/.config/t32ctl/config.toml";

fn parse(text: &str) -> Result<Config, CtlError> {
    Config::parse(text, Path::new(FILE))
}

fn refusal(text: &str) -> (String, Vec<String>) {
    let e = parse(text).unwrap_err();
    assert_eq!(e.kind, ErrorKind::ProfileInvalid);
    (e.message.replace(FILE, "F"), e.details.iter().map(|d| d.replace(FILE, "F")).collect())
}

#[test]
fn every_key_of_the_mvp_schema_is_read() {
    let c = parse(
        "schema = 1\n\
         [connection]\nhost = \"10.0.0.2\"\nport = 20001\n\
         [staging]\nvm_base = 0x02000000\nvm_len = 0x01000000\nroot = 'C:\\T32\\tmp\\t32ctl'\n\
         [tuning]\nread_chunk = 1008\nwrite_chunk = 1024\n\
         [calls]\ntransfer_timeout = \"30s\"\ntransfer_max = \"64M\"\n",
    )
    .unwrap();
    assert_eq!(c.file, Some(PathBuf::from(FILE)));
    assert_eq!((c.host.as_str(), c.port), ("10.0.0.2", 20001));
    assert_eq!((c.window.base(), c.window.len()), (0x0200_0000, 0x0100_0000));
    assert_eq!(c.stage_root.as_ref().map(PvPath::as_str), Some("C:\\T32\\tmp\\t32ctl"));
    assert_eq!((c.tuning.read_chunk(), c.tuning.write_chunk()), (1008, 1024));
    assert_eq!((c.transfer_timeout.as_ms(), c.transfer_max.bytes()), (30_000, 64 << 20));
    let env = env_at(Path::new("/w"));
    assert_eq!(c.hosts(&env), ["10.0.0.2"]);
    let auto = Config { host: "auto".into(), ..c };
    assert_eq!(
        auto.hosts(&Env { auto_hosts: vec!["127.0.0.1".into(), "172.29.32.1".into()], ..env }),
        ["127.0.0.1", "172.29.32.1"]
    );
}

#[test]
fn an_unknown_key_stops_parsing_with_its_line_and_column() {
    let (m, d) = refusal("schema = 1\n[connection]\nhots = \"auto\"\n");
    assert_eq!(m, "F:3:1: unknown field `hots`, expected `host` or `port`");
    assert!(d.is_empty());
    let (m, _) = refusal("schema = 1\n[tuning]\nmacro = \"bare\"\n");
    assert!(m.starts_with("F:3:1: unknown field `macro`, expected one of `read_chunk`, `write_chunk`"), "{m}");
    let (m, _) = refusal("schema = 1\n[connection\n");
    assert!(m.starts_with("F:2:"), "{m}");
    let (m, _) = refusal("[connection]\nport = 1\n");
    assert!(m.contains("missing field `schema`"), "{m}");
}

#[test]
fn planned_keys_are_named_as_planned() {
    let (m, d) = refusal(
        "schema = 1\n[tuning]\ngo_leaves_breakpoint = false\n[calls]\nlock_wait = \"10s\"\n[lease]\nidle = \"10m\"\n",
    );
    assert_eq!(m, "F:3:24: tuning.go_leaves_breakpoint is planned, not in this t32ctl");
    assert_eq!(d.len(), 2, "{d:?}");
    assert!(d.iter().any(|l| l == "F:5:13: calls.lock_wait is planned, not in this t32ctl"), "{d:?}");
    assert!(d.iter().any(|l| l.starts_with("F:") && l.ends_with(": [lease] is planned, not in this t32ctl")), "{d:?}");
}

#[test]
fn every_value_out_of_range_is_reported_with_its_position() {
    let (m, d) = refusal(
        "schema = 2\n\
         [connection]\nhost = \"a b\"\nport = 0\n\
         [staging]\nvm_base = 0xFFFFFFF0\nvm_len = 0x100\nroot = 'relative\\x'\n\
         [tuning]\nread_chunk = 4\nwrite_chunk = 20000\n\
         [calls]\ntransfer_timeout = \"2h\"\ntransfer_max = \"8G\"\n",
    );
    assert_eq!(m, "F:1:10: schema must be 1");
    assert_eq!(
        d,
        [
            "F:3:8: connection.host must be \"auto\" or a host name or address",
            "F:4:8: connection.port = 0 is outside 1..=65535",
            "F:7:10: staging.vm_base and staging.vm_len: VM: range 0xfffffff0+0x100 leaves the 32-bit VM: space",
            "F:8:8: staging.root: path \"relative\\\\x\" is not an absolute PowerView host path",
            "F:10:14: tuning.read_chunk = 4 is outside 8..=15360",
            "F:11:15: tuning.write_chunk = 20000 is outside 8..=15360",
            "F:13:20: calls.transfer_timeout: duration 2h is outside 1s..1h",
            "F:14:16: calls.transfer_max 8G is outside 1..4G",
        ]
    );
    let (m, _) = refusal("schema = 1\n[calls]\ntransfer_max = \"0\"\n");
    assert_eq!(m, "F:3:16: calls.transfer_max 0 is outside 1..4G");
    let (m, _) = refusal("schema = 1\n[calls]\ntransfer_timeout = \"10 m\"\n");
    assert_eq!(m, "F:3:20: calls.transfer_timeout: duration \"10 m\": use 500ms, 30s, 41.3s, 5m, 2m40s or 1h30m");
}

#[test]
fn line_and_column_count_characters() {
    assert_eq!(line_col("ab\ncd", 0), (1, 1));
    assert_eq!(line_col("ab\ncd", 4), (2, 2));
    assert_eq!(line_col("é\nxé = 1", 6), (2, 3));
    assert_eq!(line_col("x", 99), (1, 2));
}

fn cache(t: &Temp, build: &str, board_dir: Option<&str>, board: Option<&str>) {
    let mut text = String::from("# This is the CMakeCache file.\n//comment\nCMAKE_BUILD_TYPE:STRING=\n");
    if let Some(d) = board_dir {
        text.push_str(&format!("BOARD_DIR:PATH={d}\n"));
    }
    if let Some(b) = board {
        text.push_str(&format!("BOARD:STRING={b}\n"));
    }
    t.write(&format!("{build}/CMakeCache.txt"), &text);
}

fn resolve(env: &Env, flag: Option<&str>) -> Binding {
    Binding::resolve(env, flag.map(Path::new)).unwrap()
}

fn shown(env: &Env, s: &Option<Sourced<PathBuf>>) -> Option<(String, &'static str)> {
    s.as_ref().map(|s| (env.display(&s.value), s.source.text()))
}

#[test]
fn without_any_binding_nothing_is_resolved_and_the_tried_sources_are_listed() {
    let t = Temp::new("nothing");
    t.dir("build");
    let env = env_at(&t.0);
    let b = resolve(&env, None);
    assert_eq!((b.build, b.profile, b.board), (None, None, None));
    assert_eq!(
        b.tried,
        [
            "T32CTL_PROFILE: unset",
            ".t32ctl.toml: not in a git repository",
            "BOARD_DIR: no build directory (--build, T32CTL_BUILD, .t32ctl.toml, ./build)"
        ]
    );
}

#[test]
fn the_build_cache_names_the_profile_and_the_board() {
    let t = Temp::new("auto");
    let board = t.dir("boards/acme/acme_evb");
    cache(&t, "build", Some(board.to_str().unwrap()), Some("acme_evb/a53"));
    let env = env_at(&t.0);
    let b = resolve(&env, None);
    assert_eq!(shown(&env, &b.build), Some(("build".into(), "auto-detection: ./build/CMakeCache.txt")));
    assert_eq!(shown(&env, &b.profile), Some(("boards/acme/acme_evb/support/t32ctl".into(), "BOARD_DIR")));
    assert_eq!(b.board.as_deref(), Some("acme_evb/a53"));
    cache(&t, "other", None, Some("acme_evb/a53/up"));
    let b = resolve(&env, Some("other"));
    assert_eq!(shown(&env, &b.build), Some(("other".into(), "--build")));
    assert_eq!((b.profile, b.board.as_deref()), (None, Some("acme_evb/a53/up")));
    assert_eq!(b.tried.last().map(String::as_str), Some("BOARD_DIR: not in other/CMakeCache.txt"));
}

#[test]
fn the_flag_beats_the_environment_which_beats_the_binding_file() {
    let t = Temp::new("matrix");
    t.dir(".git");
    t.write(".t32ctl.toml", "schema = 1\nbuild = \"build-evb\"\nprofile = \"boards/x/support/t32ctl\"\n");
    let sub = t.dir("app/src");
    let mut env = env_at(&sub);
    let b = resolve(&env, None);
    assert_eq!(shown(&env, &b.build).map(|(_, s)| s), Some(".t32ctl.toml"));
    assert_eq!(b.build.as_ref().map(|s| s.value.clone()), Some(t.0.join("build-evb")));
    assert_eq!(
        b.profile.as_ref().map(|s| (s.value.clone(), s.source)),
        Some((t.0.join("boards/x/support/t32ctl"), Source::BindingFile))
    );
    assert_eq!(b.tried, ["T32CTL_PROFILE: unset"]);
    env.build = Some("b2".into());
    env.profile = Some("/abs/support/t32ctl/".into());
    let b = resolve(&env, None);
    assert_eq!(shown(&env, &b.build), Some(("b2".into(), "T32CTL_BUILD")));
    assert_eq!(shown(&env, &b.profile), Some(("/abs/support/t32ctl".into(), "T32CTL_PROFILE")));
    assert!(b.tried.is_empty());
    let b = resolve(&env, Some("b3"));
    assert_eq!(shown(&env, &b.build), Some(("b3".into(), "--build")));
}

#[test]
fn a_binding_file_without_a_profile_falls_through_to_the_build_cache() {
    let t = Temp::new("fallthrough");
    fs::write(t.0.join(".git"), "gitdir: elsewhere\n").unwrap();
    t.write(".t32ctl.toml", "schema = 1\nbuild = \"out\"\n");
    cache(&t, "out", Some("boards/y"), None);
    let env = env_at(&t.0);
    let b = resolve(&env, None);
    assert_eq!(shown(&env, &b.profile), Some(("out/boards/y/support/t32ctl".into(), "BOARD_DIR")));
    assert_eq!(b.tried, ["T32CTL_PROFILE: unset", ".t32ctl.toml: no profile key"]);
}

#[test]
fn a_profile_outside_support_t32ctl_or_a_broken_binding_file_is_invalid() {
    let t = Temp::new("invalid");
    let env = Env { profile: Some("boards/x/scripts".into()), ..env_at(&t.0) };
    let e = Binding::resolve(&env, None).unwrap_err();
    assert_eq!(e.kind, ErrorKind::ProfileInvalid);
    assert_eq!(
        e.message,
        "T32CTL_PROFILE: profile directory boards/x/scripts is not a board's support/t32ctl directory"
    );
    t.dir(".git");
    t.write(".t32ctl.toml", "schema = 1\nprofil = \"x\"\n");
    let e = Binding::resolve(&env_at(&t.0), None).unwrap_err();
    assert_eq!(e.kind, ErrorKind::ProfileInvalid);
    let file = t.0.join(".t32ctl.toml");
    assert!(e.message.starts_with(&format!("{}:2:1: unknown field `profil`", file.display())), "{}", e.message);
    t.write(".t32ctl.toml", "schema = 3\n");
    let e = Binding::resolve(&env_at(&t.0), None).unwrap_err();
    assert_eq!(e.message, format!("{}:1:10: schema must be 1", file.display()));
}
