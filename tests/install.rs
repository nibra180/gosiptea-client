use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn executable(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn install_replaces_the_old_launcher_without_touching_user_data() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let prefix = directory.path().join("local");
    let tools = directory.path().join("tools");
    let home = directory.path().join("home");
    fs::create_dir_all(project.join("scripts")).unwrap();
    fs::create_dir_all(project.join("packaging")).unwrap();
    fs::create_dir_all(project.join("assets/logo")).unwrap();
    fs::write(
        project.join("scripts/install-local.sh"),
        include_str!("../scripts/install-local.sh"),
    )
    .unwrap();
    fs::write(
        project.join("packaging/sippy.desktop"),
        include_str!("../packaging/sippy.desktop"),
    )
    .unwrap();
    let icon = include_bytes!("../assets/logo/sippy-icon.png");
    fs::write(project.join("assets/logo/sippy-icon.png"), icon).unwrap();
    executable(&project.join("target/release/sippy"), "#!/bin/sh\nexit 0\n");
    executable(&tools.join("cargo"), "#!/bin/sh\nexit 0\n");
    for tool in ["gtk-update-icon-cache", "update-desktop-database"] {
        executable(&tools.join(tool), "#!/bin/sh\nexit 0\n");
    }
    let old_paths = [
        "bin/gosiptea-client",
        "share/applications/gosiptea-client.desktop",
        "share/icons/hicolor/512x512/apps/gosiptea-client.png",
    ];
    for path in old_paths {
        let path = prefix.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "old client").unwrap();
    }
    executable(&prefix.join("bin/gosiptea"), "original Go installation");
    let config = home.join(".baresip");
    fs::create_dir_all(&config).unwrap();
    for name in [
        "accounts",
        "gosiptea-settings.json",
        "gosiptea-call-history.json",
    ] {
        fs::write(config.join(name), "existing user data").unwrap();
    }
    let path = format!(
        "{}:{}",
        tools.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let install = || {
        Command::new("bash")
            .arg(project.join("scripts/install-local.sh"))
            .env("PREFIX", &prefix)
            .env("HOME", &home)
            .env("PATH", &path)
            .output()
            .unwrap()
    };

    // A failed build must not remove the installed old version.
    executable(&tools.join("cargo"), "#!/bin/sh\nexit 1\n");
    assert!(!install().status.success());
    for path in old_paths {
        assert!(prefix.join(path).exists());
    }
    executable(&tools.join("cargo"), "#!/bin/sh\nexit 0\n");
    let result = install();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for path in old_paths {
        assert!(!prefix.join(path).exists());
    }
    assert_ne!(
        fs::metadata(prefix.join("bin/sippy"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert_eq!(
        fs::read(prefix.join("share/icons/hicolor/512x512/apps/sippy.png")).unwrap(),
        icon
    );
    assert!(prefix.join("share/applications/sippy.desktop").exists());
    assert!(prefix.join("bin/gosiptea").exists());
    for name in [
        "accounts",
        "gosiptea-settings.json",
        "gosiptea-call-history.json",
    ] {
        assert_eq!(
            fs::read_to_string(config.join(name)).unwrap(),
            "existing user data"
        );
    }
    assert!(fs::read_dir(prefix.join("bin")).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".sippy.")
    }));
}
