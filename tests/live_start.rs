use std::{
    fs,
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;
use zbus::blocking::{Proxy, connection::Builder};

struct PrivateDesktopProcess(Option<Child>);
impl Drop for PrivateDesktopProcess {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            // The unreaped launcher reserves this process-group ID until cleanup.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
        }
    }
}

fn wait_until<T>(mut probe: impl FnMut() -> Option<T>, description: &str) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(result) = probe() {
            return result;
        }
        assert!(Instant::now() < deadline, "timed out: {description}");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "requires a running Hyprland desktop, Vulkan, baresip and dbus-run-session"]
fn real_window_starts_without_an_account_and_shutdown_reaps_owned_baresip() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config");
    let pid_file = directory.path().join("app.pid");
    let bus_file = directory.path().join("bus.address");
    let log = fs::File::create(directory.path().join("startup.log")).unwrap();
    let mut command = Command::new("dbus-run-session");
    command.args(["--", "sh", "-c", "printf '%s' \"$$\" > \"$1\"; printf '%s' \"$DBUS_SESSION_BUS_ADDRESS\" > \"$2\"; exec \"$3\" --config-dir \"$4\"", "sh"])
        .arg(&pid_file).arg(&bus_file).arg(env!("CARGO_BIN_EXE_gosiptea-client")).arg(&config)
        .stdin(Stdio::null()).stdout(log.try_clone().unwrap()).stderr(log);
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut process = PrivateDesktopProcess(Some(command.spawn().unwrap()));
    let app_pid: u32 = wait_until(
        || fs::read_to_string(&pid_file).ok()?.parse().ok(),
        "application PID",
    );
    let address = wait_until(
        || fs::read_to_string(&bus_file).ok().filter(|s| !s.is_empty()),
        "private bus address",
    );
    assert_ne!(
        Some(&address),
        std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().as_ref()
    );
    let connection = Builder::address(address.as_str())
        .unwrap()
        .method_timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let bus = Proxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .unwrap();
    let owner: String = wait_until(
        || bus.call("GetNameOwner", &("com.github.Baresip",)).ok(),
        "private baresip owner",
    );
    let child_pid: u32 = bus.call("GetConnectionUnixProcessID", &(&owner,)).unwrap();
    let status = fs::read_to_string(format!("/proc/{child_pid}/status")).unwrap();
    assert!(status.lines().any(|line| {
        line.strip_prefix("PPid:")
            .is_some_and(|parent| parent.trim() == app_pid.to_string())
    }));
    wait_until(
        || {
            let clients = Command::new("hyprctl")
                .args(["clients", "-j"])
                .output()
                .ok()?;
            let clients: Value = serde_json::from_slice(&clients.stdout).ok()?;
            clients
                .as_array()?
                .iter()
                .find(|client| {
                    client["pid"].as_u64() == Some(u64::from(app_pid))
                        && client["mapped"] == true
                        && client["class"] == "gosiptea-client"
                        && client["title"] == "GoSipTea"
                })
                .cloned()
        },
        "mapped GPUI window",
    );
    let accounts = fs::read_to_string(config.join("accounts")).unwrap_or_default();
    assert!(
        accounts
            .lines()
            .all(|line| line.trim().is_empty() || line.trim().starts_with('#'))
    );
    assert_eq!(unsafe { libc::kill(app_pid as i32, libc::SIGTERM) }, 0);
    let status = wait_until(
        || process.0.as_mut().unwrap().try_wait().unwrap(),
        "graceful application shutdown",
    );
    process.0.take();
    assert!(status.success(), "application exited with {status}");
    assert_eq!(unsafe { libc::kill(child_pid as i32, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}
