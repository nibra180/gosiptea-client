// Compile the adapter here too so private parsers and process guards can be tested in isolation.
include!("../src/platform.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::TempDir;

    struct PrivateBus {
        directory: TempDir,
        daemon: Child,
        address: String,
    }

    impl PrivateBus {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let address = format!("unix:path={}/bus", directory.path().display());
            let mut daemon = Command::new("dbus-daemon")
                .args([
                    "--session",
                    "--nofork",
                    "--nopidfile",
                    "--print-address=1",
                    &format!("--address={address}"),
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut ready = String::new();
            BufReader::new(daemon.stdout.take().unwrap())
                .read_line(&mut ready)
                .unwrap();
            assert!(ready.starts_with(&address));
            Self {
                directory,
                daemon,
                address,
            }
        }

        fn connection(&self) -> Connection {
            self.connection_with_timeout(Duration::from_millis(300))
        }

        fn connection_with_timeout(&self, timeout: Duration) -> Connection {
            Builder::address(self.address.as_str())
                .unwrap()
                .method_timeout(normalized_timeout(timeout, CALL_TIMEOUT))
                .build()
                .unwrap()
        }

        fn fake_script(&self) -> PathBuf {
            let script = self.directory.path().join("fake-baresip");
            let executable = std::env::current_exe().unwrap();
            let content = format!(
                "#!/bin/sh\nexport GOSIPTEA_PLATFORM_TEST_BUS={}\nexport GOSIPTEA_PLATFORM_TEST_DIR={}\nexec {} --exact tests::fake_baresip_child --nocapture\n",
                shell_quote(&self.address),
                shell_quote(self.directory.path().to_str().unwrap()),
                shell_quote(executable.to_str().unwrap()),
            );
            std::fs::write(&script, content).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            script
        }

        fn start(&self) -> RealBackend {
            RealBackend::start_connected(
                self.connection(),
                self.directory.path(),
                &self.fake_script(),
                None,
                false,
            )
            .unwrap()
        }

        fn start_with_timeouts(
            &self,
            command_timeout: Duration,
            stop_timeout: Duration,
        ) -> RealBackend {
            RealBackend::start_connected_with_timeout(
                self.connection_with_timeout(command_timeout),
                self.directory.path(),
                &self.fake_script(),
                None,
                false,
                stop_timeout,
            )
            .unwrap()
        }

        fn assert_lock_free(&self) {
            let connection = self.connection();
            let proxy = Proxy::new(&connection, BUS, BUS_PATH, BUS).unwrap();
            let result: u32 = proxy.call("RequestName", &(STARTUP_LOCK, 4u32)).unwrap();
            assert_eq!(result, 1);
        }
    }

    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.daemon.kill();
            let _ = self.daemon.wait();
        }
    }

    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }

    struct FakeBaresip {
        directory: PathBuf,
        quit: Arc<AtomicBool>,
    }

    #[zbus::interface(name = "com.github.Baresip")]
    impl FakeBaresip {
        #[zbus(name = "invoke")]
        fn invoke(&self, command: &str) -> String {
            let mut log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.directory.join("calls"))
                .unwrap();
            writeln!(log, "{command}").unwrap();
            if command == "quit" {
                if self.directory.join("hang-quit").exists() {
                    thread::sleep(Duration::from_secs(10));
                } else {
                    self.quit.store(true, Ordering::SeqCst);
                }
            }
            if command == "test_sleep" {
                thread::sleep(Duration::from_secs(2));
            }
            command.to_owned()
        }
    }

    #[test]
    fn fake_baresip_child() {
        let Ok(address) = std::env::var("GOSIPTEA_PLATFORM_TEST_BUS") else {
            return;
        };
        let directory = PathBuf::from(std::env::var("GOSIPTEA_PLATFORM_TEST_DIR").unwrap());
        assert!(address.contains(directory.to_str().unwrap()));
        if directory.join("ignore-term").exists() {
            assert_ne!(
                unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) },
                libc::SIG_ERR
            );
        }
        let _descendant = directory.join("spawn-descendant").exists().then(|| {
            let child = Command::new("/bin/sleep").arg("20").spawn().unwrap();
            std::fs::write(directory.join("descendant-pid"), child.id().to_string()).unwrap();
            child
        });
        let startup_event = std::fs::read_to_string(directory.join("startup-event")).ok();
        let quit = Arc::new(AtomicBool::new(false));
        let connection = Builder::address(address.as_str())
            .unwrap()
            .serve_at(
                "/baresip",
                FakeBaresip {
                    directory: directory.clone(),
                    quit: quit.clone(),
                },
            )
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .build()
            .unwrap();
        if let Some(payload) = startup_event {
            connection
                .emit_signal(
                    None::<&str>,
                    "/baresip",
                    SERVICE,
                    "event",
                    &("call", "CALL_INCOMING", payload),
                )
                .unwrap();
        }
        std::fs::write(directory.join("child-pid"), std::process::id().to_string()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !quit.load(Ordering::SeqCst) && Instant::now() < deadline {
            let event = directory.join("emit");
            if let Ok(payload) = std::fs::read_to_string(&event) {
                std::fs::remove_file(event).unwrap();
                connection
                    .emit_signal(
                        None::<&str>,
                        "/baresip",
                        SERVICE,
                        "event",
                        &("call", "CALL_INCOMING", payload),
                    )
                    .unwrap();
            }
            if directory.join("release").exists() {
                connection.release_name(SERVICE).unwrap();
                std::fs::remove_file(directory.join("release")).unwrap();
            }
            thread::sleep(TICK);
        }
    }

    fn eventually(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !predicate() {
            assert!(Instant::now() < deadline, "condition timed out");
            thread::sleep(TICK);
        }
    }

    #[test]
    fn timeout_defaults_and_small_positive_values() {
        assert_eq!(CALL_TIMEOUT, Duration::from_secs(10));
        assert_eq!(STOP_TIMEOUT, Duration::from_secs(5));
        assert_eq!(
            normalized_timeout(Duration::ZERO, CALL_TIMEOUT),
            CALL_TIMEOUT
        );
        assert_eq!(
            normalized_timeout(Duration::ZERO, STOP_TIMEOUT),
            STOP_TIMEOUT
        );
        assert_eq!(
            normalized_timeout(Duration::from_nanos(1), CALL_TIMEOUT),
            Duration::from_millis(1)
        );
        assert_eq!(
            normalized_timeout(Duration::from_millis(25), CALL_TIMEOUT),
            Duration::from_millis(25)
        );
        let bus = PrivateBus::new();
        let mut backend = bus.start_with_timeouts(Duration::ZERO, Duration::ZERO);
        assert_eq!(command_timeout(&backend.connection), CALL_TIMEOUT);
        assert_eq!(backend.stop_timeout, STOP_TIMEOUT);
        backend.shutdown().unwrap();
        let mut backend = bus.start();
        assert_eq!(
            command_timeout(&backend.connection),
            Duration::from_millis(300)
        );
        assert_eq!(backend.stop_timeout, STOP_TIMEOUT);
        backend.shutdown().unwrap();
    }

    #[test]
    fn validates_commands_like_go() {
        assert_eq!(build_command("accept", "").unwrap(), "accept");
        assert_eq!(
            build_command("hangup", "scode=603 reason=Decline").unwrap(),
            "hangup scode=603 reason=Decline"
        );
        for name in ["", "Dial", "reg-info", "x1", "dial accept", &"a".repeat(33)] {
            assert!(build_command(name, "").is_err());
        }
        for params in ["a\nb", "a\tb", "a\0b", &"a".repeat(2049)] {
            assert!(build_command("dial", params).is_err());
        }
        assert!(build_command("audio_debug", "é").is_ok());
        assert!(build_command("dial", &"a".repeat(2048)).is_ok());
        assert_eq!(truncate_utf8("aa€", 4), "aa");
    }

    #[test]
    fn maps_go_events_and_rejects_malformed_payloads() {
        let event = parse_event(r#"{"type":"CALL_INCOMING","id":"7","peeruri":"sip:1@example.test","peerdisplay":"Alice"}"#).unwrap();
        assert_eq!(event.id, "7");
        assert_eq!(event.peer_display_name, "Alice");
        assert_eq!(event.peer_uri, "sip:1@example.test");
        let event = parse_event(
            r#"{"type":"REGISTER_FAIL","accountaor":"sip:a@example.test","param":"403 Forbidden"}"#,
        )
        .unwrap();
        assert_eq!(event.account_aor, "sip:a@example.test");
        assert_eq!(event.detail, "403 Forbidden");
        assert!(parse_event(r#"{"type":"UNREGISTERING"}"#).is_ok());
        for payload in [
            "null",
            "[]",
            "{",
            "{}",
            r#"{"type":"CALL_INCOMING","id":7}"#,
            r#"{"type":"OTHER"}"#,
            &"x".repeat(MAX_EVENT + 1),
        ] {
            assert!(parse_event(payload).is_err(), "{payload}");
        }
    }

    #[test]
    fn notifications_are_escaped_bounded_and_not_shell_commands() {
        assert_eq!(
            notification_text("Alice & Bob\r\n<call>\0", 256),
            "Alice &amp; Bob\n&lt;call&gt;"
        );
        assert_eq!(
            notification_text("--help $(touch /tmp/nope)", 256),
            "--help $(touch /tmp/nope)"
        );
        assert_eq!(notification_text("é<&", 7), "é&lt;");
        assert!(notification_text(&"<&é".repeat(300), 256).len() <= 256);
        assert_eq!(
            player_names(vec![
                "other".into(),
                "org.mpris.MediaPlayer2.vlc".into(),
                "org.mpris.MediaPlayer2.a".into(),
                "org.mpris.MediaPlayer2.vlc".into()
            ]),
            ["org.mpris.MediaPlayer2.a", "org.mpris.MediaPlayer2.vlc"]
        );
    }

    #[test]
    fn audio_discovery_includes_filters_and_skips_internal_nodes() {
        let mut calls = Vec::new();
        let nodes = discover_audio_with(|args| {
            calls.push(args.join(" "));
            Ok(match args {
                ["list", "audio"] => "34\tspeaker\taudio/sink\t*\n99\tinternal\taudio/source\t \n",
                ["status", "-n"] => "Audio\n ├─ Filters:\n │ * 102. steinberg.input [Audio/Source]\n │ 103. hidden [Audio/Source/Internal]\n └─ Streams:\n",
                ["inspect", "34"] => "node.name = \"speaker\"\nmedia.class = \"Audio/Sink\"\nnode.description = \"Speakers\"\n",
                ["inspect", "99"] => "node.name = \"internal\"\nmedia.class = \"Audio/Source/Internal\"\n",
                ["inspect", "102"] => "node.name = \"steinberg.input\"\nmedia.class = \"Audio/Source\"\nnode.nick = \"Input 1\"\n",
                _ => panic!("unexpected command: {args:?}"),
            }.into())
        }).unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].description, "Speakers");
        assert_eq!(nodes[1].description, "Input 1");
        assert_eq!(nodes[1].kind, NodeKind::Input);
        assert!(nodes[1].is_default);
        assert_eq!(
            calls,
            [
                "list audio",
                "status -n",
                "inspect 34",
                "inspect 99",
                "inspect 102"
            ]
        );
    }

    #[test]
    fn audio_rejects_node_reuse_and_malformed_output() {
        let error = discover_audio_with(|args| {
            Ok(match args {
                ["list", "audio"] => "42\told\taudio/source\t\n",
                ["status", "-n"] => "Audio\n ├─ Filters:\n │ 42. new [Audio/Source]\n",
                _ => panic!("inspection must not run"),
            }
            .into())
        })
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("changed between list and status")
        );
        let listed = parse_audio_list("42\told\taudio/source\t\n")
            .unwrap()
            .remove(0);
        assert!(
            inspect_node(
                "node.name = \"new\"\nmedia.class = \"Audio/Source\"\n",
                listed.node
            )
            .is_err()
        );
        for text in [
            "42\tfoo\taudio/sink\t*\textra",
            "42\tfoo\taudio/sink\t!",
            "42\tfoo\taudio/sink\t\n42\tfoo\taudio/sink\t",
            "no tabs",
        ] {
            assert!(parse_audio_list(text).is_err());
        }
        assert!(parse_audio_filters("not Audio").is_err());
        assert!(
            parse_audio_filters("Audio\nFilters:\n42 missing separator [Audio/Source]").is_err()
        );
        assert!(parse_audio_list(&format!("42\t{}\taudio/sink\t", "x".repeat(64 * 1024))).is_err());
        assert_eq!(decode_property(r#""a\x20b\040c\u00e9""#).unwrap(), "a b cé");
    }

    #[test]
    fn focus_prefers_gpui_pid_and_falls_back_without_terminal_requirement() {
        let mut calls = Vec::new();
        focus_with(
            500,
            |_| panic!("must focus GPUI before walking ancestors"),
            |args| {
                calls.push(args.join(" "));
                match args {
                    ["-j", "clients"] => Ok(
                        r#"[{"address":"0x123","pid":100},{"address":"0x456","pid":500}]"#.into(),
                    ),
                    ["dispatch", lua] if lua.starts_with("hl.dsp") => bail!("old Hyprland"),
                    ["dispatch", "focuswindow", "address:0x456"] => Ok(String::new()),
                    _ => panic!("unexpected {args:?}"),
                }
            },
        )
        .unwrap();
        assert_eq!(calls.len(), 3);
        assert!(!valid_address("0x1\" }); evil()"));
        assert!(!valid_address("0x+1"));
        let mut focused = false;
        focus_with(
            500,
            |_| Ok(300),
            |args| {
                if args == ["-j", "clients"] {
                    return Ok(r#"[{"address":"0xabc","pid":300}]"#.into());
                }
                focused = true;
                Ok(String::new())
            },
        )
        .unwrap();
        assert!(focused);
    }

    #[test]
    fn logs_are_private_and_refuse_symlinks_or_hardlinks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        drop(open_log(&path).unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        drop(open_log(&path).unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        symlink(&path, dir.path().join("link")).unwrap();
        assert!(open_log(&dir.path().join("link")).is_err());
        std::fs::hard_link(&path, dir.path().join("hardlink")).unwrap();
        assert!(open_log(&path).is_err());
        assert!(RealBackend::start(dir.path(), Path::new("/must/not/run"), None, true).is_err());
    }

    #[test]
    fn existing_owner_is_refused_before_executing_anything() {
        let bus = PrivateBus::new();
        let foreign = bus.connection();
        foreign.request_name(SERVICE).unwrap();
        let result = RealBackend::start_connected(
            bus.connection(),
            bus.directory.path(),
            Path::new("/must/not/run"),
            None,
            false,
        );
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("another process owns")
        );
        bus.assert_lock_free();
        let proxy = Proxy::new(&foreign, BUS, BUS_PATH, BUS).unwrap();
        let owner: String = proxy.call("GetNameOwner", &(SERVICE,)).unwrap();
        assert_eq!(owner, foreign.unique_name().unwrap().as_str());
    }

    #[test]
    fn startup_lock_matches_go_and_spawn_failure_releases_it() {
        let bus = PrivateBus::new();
        let lock = bus.connection();
        let proxy = Proxy::new(&lock, BUS, BUS_PATH, BUS).unwrap();
        let reply: u32 = proxy
            .call("RequestName", &("com.github.GoSipTea.OwnedProcess", 4u32))
            .unwrap();
        assert_eq!(reply, 1);
        let result = RealBackend::start_connected(
            bus.connection(),
            bus.directory.path(),
            Path::new("/must/not/run"),
            None,
            false,
        );
        assert!(result.err().unwrap().to_string().contains("startup lock"));
        drop(proxy);
        lock.close().unwrap();
        let result = RealBackend::start_connected(
            bus.connection(),
            bus.directory.path(),
            Path::new("/must/not/run"),
            None,
            false,
        );
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("spawn owned child")
        );
        bus.assert_lock_free();
    }

    #[test]
    fn owned_child_commands_events_and_graceful_shutdown() {
        let bus = PrivateBus::new();
        let mut backend = bus.start();
        assert_eq!(backend.command("reginfo", "").unwrap(), "reginfo");
        assert!(backend.command("dial", "bad\nargument").is_err());
        let before = Instant::now();
        assert!(backend.poll_events().unwrap().is_empty());
        assert!(before.elapsed() < Duration::from_millis(100));
        std::fs::write(
            bus.directory.path().join("emit"),
            r#"{"type":"CALL_INCOMING","id":"7","peerdisplayname":"Alice"}"#,
        )
        .unwrap();
        let mut events = Vec::new();
        eventually(|| {
            events.extend(backend.poll_events().unwrap());
            !events.is_empty()
        });
        assert_eq!(events[0].peer_display_name, "Alice");
        backend.shutdown().unwrap();
        backend.shutdown().unwrap();
        let calls = std::fs::read_to_string(bus.directory.path().join("calls")).unwrap();
        assert_eq!(calls, "reginfo\nquit\n");
        assert!(backend.command("reginfo", "").is_err());
        bus.assert_lock_free();
    }

    #[test]
    fn immediate_child_event_survives_startup() {
        let bus = PrivateBus::new();
        std::fs::write(
            bus.directory.path().join("startup-event"),
            r#"{"type":"CALL_INCOMING","id":"early","peerdisplayname":"Alice"}"#,
        )
        .unwrap();
        let mut backend = bus.start();
        let mut events = Vec::new();
        eventually(|| {
            events.extend(backend.poll_events().unwrap());
            !events.is_empty()
        });
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "early");
        assert_eq!(events[0].peer_display_name, "Alice");
        assert!(!bus.directory.path().join("calls").exists());
        backend.shutdown().unwrap();
    }

    #[test]
    fn owner_loss_closes_backend_and_never_commands_replacement() {
        let bus = PrivateBus::new();
        let mut backend = bus.start();
        std::fs::write(bus.directory.path().join("release"), "").unwrap();
        eventually(|| backend.poll_events().is_err());
        let replacement = bus.connection();
        replacement.request_name(SERVICE).unwrap();
        assert!(backend.command("quit", "").is_err());
        backend.shutdown().unwrap();
        assert!(!bus.directory.path().join("calls").exists());
        bus.assert_lock_free();
    }

    #[test]
    fn commands_are_pinned_even_before_owner_change_is_processed() {
        let bus = PrivateBus::new();
        let mut backend = bus.start();
        // Suppress owner-loss cleanup to exercise a command racing the handoff.
        state_lock(&backend.state).stopping = true;
        std::fs::write(bus.directory.path().join("release"), "").unwrap();
        eventually(|| !bus.directory.path().join("release").exists());
        let replacement = bus.connection();
        replacement.request_name(SERVICE).unwrap();
        assert_eq!(backend.invoke("reginfo").unwrap(), "reginfo");
        backend.finish(false).unwrap();
        assert_eq!(
            std::fs::read_to_string(bus.directory.path().join("calls")).unwrap(),
            "reginfo\n"
        );
    }

    #[test]
    fn foreign_pid_after_preflight_is_refused_without_a_command() {
        let bus = PrivateBus::new();
        let script = bus.directory.path().join("wait-child");
        let started = bus.directory.path().join("started");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ntouch {}\nexec /bin/sleep 20\n",
                shell_quote(started.to_str().unwrap())
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let foreign = bus.connection();
        let directory = bus.directory.path().to_owned();
        let connection = bus.connection();
        let starter = thread::spawn(move || {
            RealBackend::start_connected(connection, &directory, &script, None, false)
        });
        eventually(|| started.exists());
        foreign.request_name(SERVICE).unwrap();
        let error = starter.join().unwrap().err().unwrap();
        assert!(error.to_string().contains("PID does not match"), "{error}");
        assert!(!bus.directory.path().join("calls").exists());
        bus.assert_lock_free();
    }

    #[test]
    fn command_timeout_does_not_block_forever() {
        let bus = PrivateBus::new();
        let mut backend = bus.start_with_timeouts(Duration::from_millis(60), STOP_TIMEOUT);
        let before = Instant::now();
        assert!(backend.command("test_sleep", "").is_err());
        assert!(before.elapsed() < Duration::from_millis(250));
        assert_eq!(
            std::fs::read_to_string(bus.directory.path().join("calls")).unwrap(),
            "test_sleep\n"
        );
    }

    fn child_pid(bus: &PrivateBus, file: &str) -> i32 {
        let path = bus.directory.path().join(file);
        eventually(|| path.exists());
        std::fs::read_to_string(path).unwrap().parse().unwrap()
    }

    fn process_dead(pid: i32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|stat| stat.rsplit_once(')').unwrap().1.split_whitespace().next() == Some("Z"))
            .unwrap_or(true)
    }

    #[test]
    fn hanging_quit_shares_stop_budget_and_cleans_only_owned_group() {
        for command_timeout in [Duration::from_secs(2), Duration::from_millis(60)] {
            let bus = PrivateBus::new();
            for flag in ["hang-quit", "ignore-term", "spawn-descendant"] {
                std::fs::write(bus.directory.path().join(flag), "").unwrap();
            }
            let systemd = bus.connection();
            systemd.request_name("org.freedesktop.systemd1").unwrap();
            let messages = MessageIterator::from(&systemd);
            let service_calls = thread::spawn(move || {
                messages
                    .filter(|message| {
                        message.as_ref().is_ok_and(|message| {
                            message.header().message_type() == zbus::message::Type::MethodCall
                        })
                    })
                    .count()
            });
            let mut unrelated_command = Command::new("/bin/sleep");
            unrelated_command.arg("20");
            configure_child(&mut unrelated_command);
            let unrelated = OwnedChild(Some(unrelated_command.spawn().unwrap()));
            let stop_timeout = Duration::from_millis(240);
            let mut backend = bus.start_with_timeouts(command_timeout, stop_timeout);
            let pid = child_pid(&bus, "child-pid");
            let descendant = child_pid(&bus, "descendant-pid");
            assert_eq!(unsafe { libc::getpgid(descendant) }, pid);
            let before = Instant::now();
            backend.shutdown().unwrap();
            assert!(
                before.elapsed() < stop_timeout + Duration::from_millis(150),
                "shutdown took {:?}",
                before.elapsed()
            );
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            eventually(|| process_dead(descendant));
            assert!(!unrelated.exited().unwrap());
            assert_eq!(
                std::fs::read_to_string(bus.directory.path().join("calls")).unwrap(),
                "quit\n"
            );
            assert!(state_lock(&backend.state).finished);
            bus.assert_lock_free();
            systemd.close().unwrap();
            assert_eq!(service_calls.join().unwrap(), 0);
        }
    }

    #[test]
    fn supervisor_fault_cleanup_uses_configured_stop_budget() {
        for disconnect in [false, true] {
            let mut bus = PrivateBus::new();
            for flag in ["ignore-term", "spawn-descendant"] {
                std::fs::write(bus.directory.path().join(flag), "").unwrap();
            }
            let stop_timeout = Duration::from_millis(120);
            let mut backend = bus.start_with_timeouts(Duration::from_secs(2), stop_timeout);
            let pid = child_pid(&bus, "child-pid");
            let descendant = child_pid(&bus, "descendant-pid");
            let before = Instant::now();
            if disconnect {
                bus.daemon.kill().unwrap();
                bus.daemon.wait().unwrap();
            } else {
                std::fs::write(bus.directory.path().join("release"), "").unwrap();
            }
            eventually(|| state_lock(&backend.state).finished && backend.connection.is_closed());
            assert!(
                before.elapsed() < stop_timeout + Duration::from_millis(150),
                "fault cleanup took {:?}",
                before.elapsed()
            );
            assert!(backend.poll_events().is_err());
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            eventually(|| process_dead(descendant));
            assert!(!bus.directory.path().join("calls").exists());
            backend.shutdown().unwrap();
            if !disconnect {
                bus.assert_lock_free();
            }
        }
    }

    #[test]
    fn exited_child_cleanup_kills_descendants_before_releasing_lock() {
        let bus = PrivateBus::new();
        for flag in ["ignore-term", "spawn-descendant"] {
            std::fs::write(bus.directory.path().join(flag), "").unwrap();
        }
        let mut backend =
            bus.start_with_timeouts(Duration::from_secs(2), Duration::from_millis(120));
        let pid = child_pid(&bus, "child-pid");
        let descendant = child_pid(&bus, "descendant-pid");
        let before = Instant::now();
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        eventually(|| state_lock(&backend.state).finished && backend.connection.is_closed());
        assert!(before.elapsed() < Duration::from_millis(270));
        eventually(|| process_dead(descendant));
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert!(!bus.directory.path().join("calls").exists());
        backend.shutdown().unwrap();
        bus.assert_lock_free();
    }

    #[test]
    fn bus_disconnect_terminates_owned_child() {
        let mut bus = PrivateBus::new();
        let mut backend = bus.start();
        bus.daemon.kill().unwrap();
        bus.daemon.wait().unwrap();
        eventually(|| backend.poll_events().is_err());
        backend.shutdown().unwrap();
        assert!(state_lock(&backend.state).finished);
    }

    #[test]
    fn child_exit_releases_lock_without_waiting_for_poll() {
        let bus = PrivateBus::new();
        let backend = bus.start();
        let pid: i32 = std::fs::read_to_string(bus.directory.path().join("child-pid"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        eventually(|| state_lock(&backend.state).finished && backend.connection.is_closed());
        bus.assert_lock_free();
    }

    #[test]
    fn process_commands_have_deadlines_and_bounded_output() {
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            run_command("/bin/printf", &["hello"], deadline, 10).unwrap(),
            "hello"
        );
        assert!(run_command("/bin/printf", &["too much"], deadline, 2).is_err());
        let before = Instant::now();
        assert!(
            run_command(
                "/bin/sleep",
                &["5"],
                Instant::now() + Duration::from_millis(50),
                10
            )
            .is_err()
        );
        assert!(before.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn process_group_escalates_when_term_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(format!(
            "trap '' TERM; touch {}; while :; do sleep 10; done",
            shell_quote(ready.to_str().unwrap())
        ));
        configure_child(&mut command);
        let mut child = OwnedChild(Some(command.spawn().unwrap()));
        eventually(|| ready.exists());
        let pid = child.pid();
        let before = Instant::now();
        child.terminate(Duration::from_millis(50)).unwrap();
        assert!(before.elapsed() < Duration::from_secs(1));
        assert!(child.0.is_none());
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    }

    #[test]
    fn loss_of_the_go_startup_lock_terminates_child() {
        let bus = PrivateBus::new();
        let mut backend = bus.start();
        let proxy = Proxy::new(&backend.connection, BUS, BUS_PATH, BUS).unwrap();
        let _: u32 = proxy.call("ReleaseName", &(STARTUP_LOCK,)).unwrap();
        eventually(|| backend.poll_events().is_err());
        backend.shutdown().unwrap();
        assert!(state_lock(&backend.state).finished);
        bus.assert_lock_free();
    }

    #[test]
    fn signal_sender_validation_variants_and_overflow() {
        let state = Arc::new(Mutex::new(BusState {
            owner: ":1.42".into(),
            ..BusState::default()
        }));
        let payload = r#"{"type":"CALL_CLOSED","id":"7"}"#;
        let message = |sender: &str| {
            zbus::Message::signal("/baresip", SERVICE, "event")
                .unwrap()
                .sender(sender)
                .unwrap()
                .build(&("call", "CALL_CLOSED", zbus::zvariant::Value::from(payload)))
                .unwrap()
        };
        handle_message(&state, &message(":1.43"));
        assert!(state_lock(&state).events.is_empty());
        let forged = zbus::Message::signal(BUS_PATH, BUS, "NameOwnerChanged")
            .unwrap()
            .sender(":1.43")
            .unwrap()
            .build(&(SERVICE, ":1.42", ":1.43"))
            .unwrap();
        handle_message(&state, &forged);
        assert!(state_lock(&state).fault.is_none());
        let message = message(":1.42");
        handle_message(&state, &message);
        assert_eq!(state_lock(&state).events[0].id, "7");
        for _ in 0..MAX_EVENTS {
            handle_message(&state, &message);
        }
        assert!(
            state_lock(&state)
                .fault
                .as_ref()
                .unwrap()
                .contains("overflow")
        );
        assert_eq!(state_lock(&state).events.len(), MAX_EVENTS);
        assert_eq!(decode_property(r#""\xc3\xa9\303\251""#).unwrap(), "éé");
    }

    fn event_signal(sender: &str, id: &str) -> zbus::Message {
        zbus::Message::signal("/baresip", SERVICE, "event")
            .unwrap()
            .sender(sender)
            .unwrap()
            .build(&(
                "call",
                "CALL_INCOMING",
                format!(r#"{{"type":"CALL_INCOMING","id":"{id}"}}"#),
            ))
            .unwrap()
    }

    #[test]
    fn startup_events_wait_for_pin_and_discard_foreign_senders_in_order() {
        let state = Arc::new(Mutex::new(BusState::default()));
        handle_message(&state, &event_signal(":1.43", "foreign"));
        handle_message(&state, &event_signal(":1.42", "first"));
        handle_message(&state, &event_signal(":1.44", "foreign"));
        handle_message(&state, &event_signal(":1.42", "second"));
        handle_message(&state, &event_signal(BUS, "not-unique"));
        assert!(state_lock(&state).events.is_empty());
        assert!(state_lock(&state).owner.is_empty());
        assert_eq!(state_lock(&state).startup_events.len(), 4);
        pin_verified_owner(&state, ":1.42").unwrap();
        handle_message(&state, &event_signal(":1.42", "third"));
        handle_message(&state, &event_signal(":1.43", "foreign"));
        assert!(pin_verified_owner(&state, ":1.43").is_err());
        let state = state_lock(&state);
        assert!(state.startup_events.is_empty());
        assert_eq!(
            state
                .events
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
    }

    #[test]
    fn startup_event_overflow_fails_closed_even_for_foreign_senders() {
        let state = Arc::new(Mutex::new(BusState::default()));
        for _ in 0..MAX_EVENTS {
            handle_message(&state, &event_signal(":1.43", "foreign"));
        }
        assert!(state_lock(&state).fault.is_none());
        handle_message(&state, &event_signal(":1.42", "owned"));
        let error = pin_verified_owner(&state, ":1.42").unwrap_err();
        assert!(error.to_string().contains("overflow"));
        let state = state_lock(&state);
        assert!(state.owner.is_empty());
        assert!(state.events.is_empty());
        assert_eq!(state.startup_events.len(), MAX_EVENTS);
    }

    #[test]
    fn startup_owner_loss_prevents_draining_buffered_events() {
        let state = Arc::new(Mutex::new(BusState::default()));
        handle_message(&state, &event_signal(":1.42", "owned"));
        let loss = zbus::Message::signal(BUS_PATH, BUS, "NameOwnerChanged")
            .unwrap()
            .sender(BUS)
            .unwrap()
            .build(&(SERVICE, ":1.42", ""))
            .unwrap();
        handle_message(&state, &loss);
        let error = pin_verified_owner(&state, ":1.42").unwrap_err();
        assert!(error.to_string().contains("disappeared during startup"));
        assert!(state_lock(&state).events.is_empty());
        assert!(state_lock(&state).owner.is_empty());
    }

    struct FakePlayer(Arc<std::sync::atomic::AtomicUsize>);

    #[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
    impl FakePlayer {
        fn pause(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn pauses_all_private_bus_players_without_resuming() {
        let bus = PrivateBus::new();
        let mut backend = bus.start();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut players = Vec::new();
        for name in ["org.mpris.MediaPlayer2.a", "org.mpris.MediaPlayer2.b"] {
            let connection = bus.connection();
            connection
                .object_server()
                .at("/org/mpris/MediaPlayer2", FakePlayer(count.clone()))
                .unwrap();
            connection.request_name(name).unwrap();
            players.push(connection);
        }
        backend.pause_media().unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 2);
        backend.shutdown().unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    struct SlowPlayer(mpsc::Sender<()>);

    #[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
    impl SlowPlayer {
        fn pause(&self) {
            let _ = self.0.send(());
            thread::sleep(Duration::from_secs(2));
        }
    }

    fn slow_player(bus: &PrivateBus) -> (Connection, mpsc::Receiver<()>) {
        let (entered, receiver) = mpsc::channel();
        let player = bus.connection();
        player
            .object_server()
            .at("/org/mpris/MediaPlayer2", SlowPlayer(entered))
            .unwrap();
        player.request_name("org.mpris.MediaPlayer2.slow").unwrap();
        (player, receiver)
    }

    #[test]
    fn effects_run_without_backend_command_mutex_or_blocking_events() {
        let bus = PrivateBus::new();
        let backend = Mutex::new(bus.start_with_timeouts(Duration::from_secs(3), STOP_TIMEOUT));
        let (_player, entered) = slow_player(&bus);
        let mut guard = backend.lock().unwrap();
        let mut effects = guard.desktop_effects().unwrap();
        let worker = thread::spawn(move || effects.pause_media());
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!worker.is_finished());
        let before = Instant::now();
        assert_eq!(guard.command("reginfo", "").unwrap(), "reginfo");
        std::fs::write(
            bus.directory.path().join("emit"),
            r#"{"type":"CALL_INCOMING","id":"while-pausing"}"#,
        )
        .unwrap();
        eventually(|| {
            guard
                .poll_events()
                .unwrap()
                .iter()
                .any(|event| event.id == "while-pausing")
        });
        assert!(before.elapsed() < Duration::from_secs(1));
        assert!(!worker.is_finished());
        worker.join().unwrap().unwrap();
        // Dropping an adapter must not stop the owned child or close its connection.
        assert_eq!(guard.command("reginfo", "").unwrap(), "reginfo");
        guard.shutdown().unwrap();
    }

    #[test]
    fn independent_effects_inherit_command_timeout() {
        let bus = PrivateBus::new();
        let timeout = Duration::from_millis(60);
        let mut backend = bus.start_with_timeouts(timeout, STOP_TIMEOUT);
        let (_player, entered) = slow_player(&bus);
        let mut effects = backend.desktop_effects().unwrap();
        let before = Instant::now();
        let worker = thread::spawn(move || effects.pause_media());
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(worker.join().unwrap().is_err());
        assert!(before.elapsed() < Duration::from_millis(250));
        assert_eq!(backend.command("reginfo", "").unwrap(), "reginfo");
        backend.shutdown().unwrap();
    }

    #[test]
    fn shutdown_cancels_multiple_independent_effect_adapters() {
        let bus = PrivateBus::new();
        let mut backend = bus.start();
        let (_player, entered) = slow_player(&bus);
        let mut first = backend.desktop_effects().unwrap();
        let mut second = backend.desktop_effects().unwrap();
        let first = thread::spawn(move || first.pause_media());
        let second = thread::spawn(move || second.pause_media());
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        let before = Instant::now();
        backend.shutdown().unwrap();
        assert!(first.join().unwrap().is_err());
        assert!(second.join().unwrap().is_err());
        assert!(before.elapsed() < Duration::from_secs(1));
        assert!(backend.desktop_effects().unwrap().pause_media().is_err());
        bus.assert_lock_free();
    }

    #[test]
    fn parent_death_helper() {
        let Ok(path) = std::env::var("GOSIPTEA_PLATFORM_PARENT_DEATH") else {
            return;
        };
        let mut command = Command::new("/bin/sleep");
        command.arg("20");
        configure_child(&mut command);
        let child = OwnedChild(Some(command.spawn().unwrap()));
        std::fs::write(path, child.pid().to_string()).unwrap();
        thread::sleep(Duration::from_secs(20));
    }

    #[test]
    fn parent_death_kills_owned_child() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("pid");
        let mut parent = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::parent_death_helper", "--nocapture"])
            .env("GOSIPTEA_PLATFORM_PARENT_DEATH", &pid_file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        eventually(|| pid_file.exists());
        let pid: i32 = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
        parent.kill().unwrap();
        parent.wait().unwrap();
        eventually(|| {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .map(|stat| stat.rsplit_once(')').unwrap().1.split_whitespace().next() == Some("Z"))
                .unwrap_or(true)
        });
    }
}
