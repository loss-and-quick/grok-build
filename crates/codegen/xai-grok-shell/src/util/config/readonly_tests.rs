use super::*;

/// The probe is a property of the *file*, not of `~/.grok`: a writable config
/// stays writable, and clearing the owner-write bit — what a declarative
/// installer leaves behind — flips it.
#[test]
fn owner_write_bit_decides() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");

    assert!(
        !path_denies_owner_write(&path),
        "a missing config must stay writable: the first write creates it"
    );

    std::fs::write(&path, "[ui]\n").expect("write config");
    assert!(!path_denies_owner_write(&path));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))
            .expect("chmod 444");
        assert!(path_denies_owner_write(&path));
    }
}

/// home-manager installs `config.toml` as a symlink into `/nix/store`, whose
/// link mode is `lrwxrwxrwx`. The probe must follow the link to the read-only
/// target, or every nix user reads as writable and the write goes through.
#[cfg(unix)]
#[test]
fn follows_a_symlink_to_a_read_only_target() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = dir.path().join("store-config.toml");
    std::fs::write(&store, "[ui]\n").expect("write target");
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o444)).expect("chmod 444");

    let link = dir.path().join("config.toml");
    std::os::unix::fs::symlink(&store, &link).expect("symlink");

    assert_eq!(
        std::fs::symlink_metadata(&link)
            .expect("lstat")
            .permissions()
            .mode()
            & 0o200,
        0o200,
        "the link itself is writable — the probe must not stop here"
    );
    assert!(path_denies_owner_write(&link));
}

/// A dangling symlink has no target to read a mode from, and refusing there
/// would wedge the user out of their own settings with no file to edit.
#[cfg(unix)]
#[test]
fn dangling_symlink_is_not_read_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let link = dir.path().join("config.toml");
    std::os::unix::fs::symlink(dir.path().join("gone.toml"), &link).expect("symlink");
    assert!(!path_denies_owner_write(&link));
}

/// The env override answers in both directions, and an unrecognized value
/// falls through to the file probe rather than silently disarming it.
#[test]
fn env_override_parses_both_directions() {
    for on in ["1", "true", "YES", " on "] {
        assert_eq!(readonly_from_env_value(Some(on)), Some(true), "{on:?}");
    }
    for off in ["0", "false", "NO", "off"] {
        assert_eq!(readonly_from_env_value(Some(off)), Some(false), "{off:?}");
    }
    for unknown in ["", "maybe", "2"] {
        assert_eq!(readonly_from_env_value(Some(unknown)), None, "{unknown:?}");
    }
    assert_eq!(readonly_from_env_value(None), None);
}

/// The refusal has to survive `scrub_error_for_toast`, which replaces
/// anything over 120 bytes with "server error (see logs for details)" — the
/// exact opposite of what this message is for.
#[test]
fn reason_fits_a_toast() {
    for source in [ConfigReadOnly::FileMode, ConfigReadOnly::Env] {
        let subject_len = "Follow-up behavior".len();
        let len = readonly_config_reason(source).len() + subject_len + " is set in ".len();
        assert!(len <= 120, "{source:?} message is {len} bytes: {}", readonly_config_reason(source));
    }
}
