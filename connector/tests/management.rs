use std::process::{Command, Stdio};

#[test]
fn non_interactive_configuration_returns_user_command_without_collecting_secrets() {
    let root = tempfile::tempdir().unwrap();
    let run = |arguments: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tokemula-image"))
            .args(arguments)
            .env_clear()
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .env("PATH", "/usr/bin:/bin")
            .current_dir(root.path())
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let guidance = run(&["configure", "--terminal"]);
    assert_eq!(guidance.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&guidance.stdout).contains("自己的交互终端"));
    assert!(String::from_utf8_lossy(&guidance.stdout).contains("configure --terminal"));
    assert!(
        !root
            .path()
            .join(".config/tokemula-image-connector")
            .exists()
    );

    let rejected = run(&["configure", "--key", "SYNTHETIC_ARGUMENT_MUST_NOT_ECHO"]);
    assert_eq!(rejected.status.code(), Some(2));
    assert!(
        !String::from_utf8_lossy(&rejected.stdout).contains("SYNTHETIC_ARGUMENT_MUST_NOT_ECHO")
    );
    assert!(
        !String::from_utf8_lossy(&rejected.stderr).contains("SYNTHETIC_ARGUMENT_MUST_NOT_ECHO")
    );
}
