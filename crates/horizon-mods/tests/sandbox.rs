//! Loads real mods built with the SDK: the coords-hud example and a guest
//! that misbehaves on command. Needs `rustup target add wasm32-unknown-unknown`.
use horizon_mods::{
    Clock, Corner, Mods, Motion, Output, Package, Player, Snapshot, Status, manifest::componentize,
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    time::Instant,
};

const GUEST_MANIFEST: &str = r#"
id = "test_guest"
name = "Test Guest"
version = "0.1.0"
api = "0.1"
capabilities = ["chat"]
"#;

/// Builds both mods once and writes their `.hmod` files; returns the folder.
fn mods_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = root.join("target/mods-test");
        let status = Command::new(env!("CARGO"))
            .current_dir(&root)
            .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
            .args([
                "-p",
                "coords-hud",
                "-p",
                "horizon-mods-test-guest",
                "-p",
                "double-jump",
                "--target-dir",
            ])
            .arg(&target)
            .status()
            .expect("cargo runs");
        assert!(
            status.success(),
            "building the test mods needs `rustup target add wasm32-unknown-unknown`"
        );
        let built = target.join("wasm32-unknown-unknown/release");
        let dir = target.join("mods");
        std::fs::create_dir_all(&dir).unwrap();
        let example = root.join("crates/horizon-mod-sdk/examples/coords-hud/horizon-mod.toml");
        let jump = root.join("crates/horizon-mod-sdk/examples/double-jump/horizon-mod.toml");
        let mods = [
            ("coords_hud", std::fs::read_to_string(example).unwrap()),
            ("horizon_mods_test_guest", GUEST_MANIFEST.to_string()),
            ("double_jump", std::fs::read_to_string(jump).unwrap()),
        ];
        for (name, manifest) in mods {
            let core = std::fs::read(built.join(format!("{name}.wasm"))).unwrap();
            let hmod = Package::write(&manifest, &componentize(&core).unwrap()).unwrap();
            std::fs::write(dir.join(format!("{name}.hmod")), hmod).unwrap();
        }
        dir
    })
}

fn load(name: &str) -> Mods {
    Mods::load(&[mods_dir().join(name)], |_| true, |c| c == "give")
}

fn player() -> Snapshot {
    Snapshot {
        player: Some(Player {
            x: 12.5,
            y: 64.0,
            z: -3.25,
            yaw: 180.0,
            pitch: 0.0,
            health: 20.0,
            food: 20,
            dimension: "minecraft:overworld".into(),
        }),
        clock: Some(Clock {
            game_time: 100,
            day_time: 24000 + 6000,
        }),
        motion: None,
    }
}

#[test]
fn coords_hud_draws_and_answers() {
    let mut mods = load("coords_hud.hmod");
    assert_eq!(mods.entries()[0].status, Status::Running);
    assert_eq!(mods.commands().collect::<Vec<_>>(), ["where", "coords"]);
    mods.set_snapshot(player());
    mods.tick(1);
    let panel = mods.panels().next().expect("a panel after a tick");
    assert_eq!(panel.corner, Corner::TopLeft);
    let lines: Vec<&str> = panel.lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(
        lines,
        [
            "XYZ 12.5 / 64.0 / -3.2",
            "Facing North (-Z)",
            "Day 2, 12:00"
        ]
    );

    assert!(mods.command("where"));
    let chat = Output::Chat {
        name: "Coordinates HUD".into(),
        text: "12, 64, -4 in minecraft:overworld".into(),
    };
    assert_eq!(mods.drain(), [chat]);
    assert!(mods.command("coords"));
    mods.tick(2);
    assert_eq!(mods.panels().count(), 0);
    let hidden = Output::Feedback {
        text: "Coordinates panel hidden".into(),
        error: false,
    };
    assert_eq!(mods.drain(), [hidden]);
    assert!(!mods.command("gamemode creative"));
}

#[test]
fn off_mods_cost_nothing() {
    let mods = Mods::load(&[mods_dir().join("coords_hud.hmod")], |_| false, |_| false);
    assert_eq!(mods.entries()[0].status, Status::Off);
    assert!(!mods.is_running());
    assert_eq!(mods.commands().count(), 0);
}

#[test]
fn undeclared_capabilities_are_refused() {
    let dir = mods_dir().join("undeclared");
    std::fs::create_dir_all(&dir).unwrap();
    let wasm = Package::read(&mods_dir().join("horizon_mods_test_guest.hmod"))
        .unwrap()
        .wasm
        .expect("the guest has code");
    let manifest = GUEST_MANIFEST.replace("[\"chat\"]", "[]");
    std::fs::write(dir.join("horizon-mod.toml"), manifest).unwrap();
    std::fs::write(dir.join("mod.wasm"), wasm).unwrap();
    let mods = Mods::load(&[dir], |_| true, |_| false);
    let refused = Status::Failed("uses capability chat, missing from horizon-mod.toml".into());
    assert_eq!(mods.entries()[0].status, refused);
}

#[test]
fn vanilla_commands_win() {
    let mods = Mods::load(
        &[mods_dir().join("coords_hud.hmod")],
        |_| true,
        |c| c == "where",
    );
    assert_eq!(mods.commands().collect::<Vec<_>>(), ["coords"]);
}

#[test]
fn a_misbehaving_mod_is_stopped_alone() {
    for (command, reason) in [
        ("spin", "it took too long (an endless loop?)"),
        ("panic", "it crashed (a panic)"),
        ("grow", "it crashed (a panic)"),
    ] {
        let paths = [
            mods_dir().join("coords_hud.hmod"),
            mods_dir().join("horizon_mods_test_guest.hmod"),
        ];
        let mut mods = Mods::load(&paths, |_| true, |_| false);
        let started = Instant::now();
        assert!(mods.command(command));
        assert!(started.elapsed().as_secs() < 5, "/{command} was cut short");
        let stopped = Output::Stopped {
            name: "Test Guest".into(),
            reason: reason.into(),
        };
        assert_eq!(mods.drain(), [stopped], "/{command}");
        assert_eq!(mods.entries()[1].status, Status::Failed(reason.into()));
        // The other mod runs on, and the stopped one's command is gone.
        assert!(mods.is_running());
        assert!(!mods.command("hello"));
        mods.set_snapshot(player());
        mods.tick(1);
        assert_eq!(mods.panels().count(), 1);
    }
}

#[test]
fn hot_reload_restarts_a_changed_mod() {
    let dir = mods_dir().join("reload");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("guest.hmod");
    std::fs::copy(mods_dir().join("horizon_mods_test_guest.hmod"), &path).unwrap();
    let mut mods = Mods::load(std::slice::from_ref(&path), |_| true, |_| false);
    mods.tick(1);
    assert!(!mods.reload_changed(|_| false));
    // A rebuild: same bytes, newer file.
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(later)
        .unwrap();
    assert!(mods.reload_changed(|_| false));
    assert!(mods.command("hello world"));
    let chat = Output::Chat {
        name: "Test Guest".into(),
        text: "hello world after 0 ticks".into(),
    };
    assert_eq!(mods.drain(), [chat]);
}

/// `cargo test -p horizon-mods --release -- --ignored --nocapture`: the
/// cost of the per-tick and per-frame hooks (docs/mods.md, Performance).
#[test]
#[ignore]
fn hook_overhead() {
    let time = |mods: &mut Mods, frame: bool| {
        let n = 200_000;
        for i in 0..1000 {
            mods.tick(i);
        }
        let started = Instant::now();
        for i in 0..n {
            if frame {
                mods.frame(0.016);
            } else {
                mods.tick(i);
            }
        }
        started.elapsed().as_nanos() as f64 / n as f64
    };
    let mut guest = load("horizon_mods_test_guest.hmod");
    let mut hud = load("coords_hud.hmod");
    hud.set_snapshot(player());
    let mut none = Mods::default();
    println!("no mods, tick:        {:8.1} ns", time(&mut none, false));
    println!("empty tick hook:      {:8.1} ns", time(&mut guest, false));
    println!("empty frame hook:     {:8.1} ns", time(&mut guest, true));
    println!("coords-hud tick hook: {:8.1} ns", time(&mut hud, false));
    let started = Instant::now();
    let _ = load("coords_hud.hmod");
    println!(
        "load coords-hud:      {:8.1} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
}

#[test]
fn content_only_mods_load_without_an_engine() {
    let manifest = r#"
id = "ruby"
name = "Ruby"
version = "0.1.0"
api = "0.2"
side = "both"
capabilities = ["registries"]
"#;
    let block: &[u8] = br#"{"template": "minecraft:iron_block"}"#;
    let dir = mods_dir().join("content");
    std::fs::create_dir_all(&dir).unwrap();
    let ok = dir.join("ruby.hmod");
    let files: &[(&str, &[u8])] = &[("data/ruby/horizon/block/ruby_block.json", block)];
    std::fs::write(&ok, Package::write_files(manifest, None, files).unwrap()).unwrap();
    let bad = dir.join("bad.hmod");
    let files: &[(&str, &[u8])] = &[("data/minecraft/recipe/stone.json", b"{}")];
    let bad_manifest = manifest.replace("\"ruby\"", "\"bad\"");
    std::fs::write(
        &bad,
        Package::write_files(&bad_manifest, None, files).unwrap(),
    )
    .unwrap();
    let mods = Mods::load(&[ok, bad], |_| true, |_| false);
    assert_eq!(mods.entries()[0].status, Status::Running);
    assert!(matches!(&mods.entries()[1].status, Status::Failed(e) if e.contains("own namespace")));
    // Content has no hooks: the game's per-tick mod work stays skipped.
    assert!(!mods.is_running());
}

#[test]
fn server_mods_tick_and_each_side_selects_its_own_code() {
    let dir = mods_dir().join("server-side");
    std::fs::create_dir_all(&dir).unwrap();
    let guest = Package::read(&mods_dir().join("horizon_mods_test_guest.hmod")).unwrap();
    let manifest = format!("{GUEST_MANIFEST}\nside = \"server\"\n");
    let path = dir.join("test_guest.hmod");
    std::fs::write(
        &path,
        Package::write(&manifest, guest.wasm.as_ref().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        Mods::load(std::slice::from_ref(&path), |_| true, |_| false).entries()[0].status,
        Status::Off
    );
    assert_eq!(
        Mods::load_server(
            &[mods_dir().join("horizon_mods_test_guest.hmod")],
            |_| true,
            |_| false
        )
        .entries()[0]
            .status,
        Status::Off
    );
    let mut server = Mods::load_server(&[path], |_| true, |_| false);
    server.set_snapshot(Snapshot {
        player: None,
        clock: Some(Clock {
            game_time: 40,
            day_time: 6000,
        }),
        motion: None,
    });
    server.tick(40);
    assert!(server.command("hello server"));
    assert_eq!(
        server.drain(),
        [Output::Chat {
            name: "Test Guest".into(),
            text: "hello server after 1 ticks".into()
        }]
    );

    let package = Package::read(&mods_dir().join("coords_hud.hmod")).unwrap();
    let mut manifest = toml::to_string(&package.manifest).unwrap();
    manifest = manifest.replace("side = \"client\"", "side = \"both\"");
    let both = dir.join("coords_hud.hmod");
    std::fs::write(
        &both,
        Package::write(&manifest, package.wasm.as_ref().unwrap()).unwrap(),
    )
    .unwrap();
    let mut server = Mods::load_server(std::slice::from_ref(&both), |_| true, |_| false);
    assert!(server.is_running());
    // Even a caller accidentally providing a local player cannot give the
    // server guest a fictional player or a HUD surface.
    server.set_snapshot(player());
    server.tick(1);
    assert_eq!(server.panels().count(), 0);
    let mut client = Mods::load(&[both], |_| true, |_| false);
    client.set_snapshot(player());
    client.tick(1);
    assert_eq!(client.panels().count(), 1);
}

#[test]
fn approved_server_updates_keep_last_good_and_reconcile_additions_and_removals() {
    let dir = mods_dir().join("approved-server");
    std::fs::create_dir_all(&dir).unwrap();
    let guest = Package::read(&mods_dir().join("horizon_mods_test_guest.hmod")).unwrap();
    let manifest = format!("{GUEST_MANIFEST}\nside = \"server\"\n");
    let bytes = Package::write(&manifest, guest.wasm.as_ref().unwrap()).unwrap();
    let path = dir.join("test_guest.hmod");
    std::fs::write(&path, &bytes).unwrap();
    let mut server = Mods::load_server(&[], |_| true, |_| false);
    assert!(server.sync_server_directory(&dir, |_| false).unwrap());
    server.tick(1);
    assert!(!server.sync_server_directory(&dir, |_| false).unwrap());
    std::fs::write(&path, b"incomplete update").unwrap();
    assert!(!server.sync_server_directory(&dir, |_| false).unwrap());
    assert_eq!(server.entries()[0].status, Status::Running);
    assert!(server.command("hello retained"));
    assert_eq!(
        server.drain(),
        [Output::Chat {
            name: "Test Guest".into(),
            text: "hello retained after 1 ticks".into()
        }]
    );
    assert!(
        server
            .sync_server_directory(&dir.join("missing"), |_| false)
            .is_err()
    );
    assert!(server.is_running());

    // Same command names must survive replacement of their former owner.
    std::fs::write(&path, &bytes).unwrap();
    assert!(server.sync_server_directory(&dir, |_| false).unwrap());
    assert!(server.command("hello replaced"));
    assert_eq!(
        server.drain(),
        [Output::Chat {
            name: "Test Guest".into(),
            text: "hello replaced after 0 ticks".into()
        }]
    );
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(later)
        .unwrap();
    assert!(
        !server.sync_server_directory(&dir, |_| false).unwrap(),
        "identical bytes do not restart a mod"
    );
    std::fs::remove_file(path).unwrap();
    assert!(server.sync_server_directory(&dir, |_| false).unwrap());
    assert!(!server.is_running());
    assert!(!server.command("hello removed"));
}

#[test]
fn double_jump_nudges_through_the_movement_capability() {
    let mut mods = load("double_jump.hmod");
    assert!(mods.is_running(), "{:?}", mods.entries()[0].status);
    let mut at = |on_ground, jumping| {
        mods.set_snapshot(Snapshot {
            player: None,
            clock: None,
            motion: Some(Motion {
                vx: 0.3,
                vy: 0.0,
                vz: 0.0,
                on_ground,
                jumping,
                steerable: true,
                lift_left: 16,
            }),
        });
        mods.tick(0);
        mods.take_nudge()
    };
    // Jumping from the ground and holding it: the game's own jump.
    assert!(at(true, true).is_empty());
    assert!(at(false, true).is_empty());
    assert!(at(false, false).is_empty());
    // A fresh press in the air: the catch brakes against the run-up...
    let catch = at(false, true);
    assert_eq!(catch.vertical, Some(0.03));
    assert!(catch.push[0] < 0.0);
    // ...then the burst and the float, and nothing after.
    let mut rise = 0.03;
    let mut ticks = 1;
    while let Some(vy) = at(false, false).vertical {
        rise += vy;
        ticks += 1;
    }
    assert_eq!(ticks, 11);
    assert!((1.5..1.65).contains(&rise), "rise {rise}");
    assert!(at(false, true).is_empty(), "once per airtime");
    assert!(mods.command("doublejump"));
    assert_eq!(
        mods.drain(),
        [Output::Feedback {
            text: "Double jump off".into(),
            error: false
        }]
    );
}
