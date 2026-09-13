//! Real installs from GitHub. Not part of the normal test run; run by hand
//! with `cargo test --test network -- --ignored --nocapture`.

use mimirdlp::provision::{self, Component, Event, Paths, Status, UpdateCheck};

#[test]
#[ignore = "downloads about 200 MB from GitHub"]
fn installs_every_component_from_upstream() {
    let dir = std::env::temp_dir().join(format!("ytp-network-{}", std::process::id()));
    let paths = Paths::new(dir.clone());
    let mut last_percent = None;
    let mut emit = |event: Event| match event {
        Event::Log(level, text) => println!("{} {text}", level.tag()),
        Event::Transfer {
            file,
            received,
            total: Some(total),
        } => {
            let percent = received * 100 / total.max(1);
            if last_percent != Some(percent / 25) {
                last_percent = Some(percent / 25);
                println!("        {file}: {percent}% of {total} bytes");
            }
        }
        Event::Transfer { .. } => {}
        Event::TransferFinished => last_percent = None,
    };
    for component in Component::ALL {
        provision::install(component, &paths, &mut emit).unwrap();
    }
    for component in Component::ALL {
        let status = provision::inspect(component, &paths);
        println!("{}: {status:?}", component.label());
        assert!(matches!(status, Status::Installed { .. }), "{status:?}");
        let update = provision::check_update(component, &paths);
        println!("{}: {update:?}", component.label());
        assert_eq!(update, UpdateCheck::Current);
    }
    let leftovers: Vec<_> = std::fs::read_dir(&paths.bin_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with('.') && n != ".install_state")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    std::fs::remove_dir_all(dir).unwrap();
}
