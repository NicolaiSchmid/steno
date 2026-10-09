//! The panic hook end to end, in a process of its own: a hook is global,
//! so it would catch every other test's panics. A panic inside
//! `crash_log::expected`, which its caller catches, leaves no file and
//! does not reach the previous hook.

#[test]
fn a_panic_leaves_a_crash_log_and_still_reaches_the_previous_hook() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("support");
    let previous_ran = std::sync::Arc::new(AtomicUsize::new(0));
    {
        let previous_ran = previous_ran.clone();
        std::panic::set_hook(Box::new(move |_| {
            previous_ran.fetch_add(1, Ordering::SeqCst);
        }));
    }
    steno_core::crash_log::install_crash_log_hook(folder.clone(), None);

    let caught = std::thread::Builder::new()
        .name("steno-crash-test".into())
        .spawn(|| panic!("the meeting list ran out of rows"))
        .unwrap()
        .join();
    assert!(caught.is_err());
    let expected = std::thread::spawn(|| {
        std::panic::catch_unwind(|| {
            steno_core::crash_log::expected(|| panic!("a packet the decoder catches"))
        })
        .is_err()
    })
    .join();
    assert!(matches!(expected, Ok(true)), "caught inside the thread");
    // The assertions below report through the default hook again.
    drop(std::panic::take_hook());

    let logs: Vec<_> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(logs.len(), 1, "{logs:?}");
    let name = logs[0].file_name().unwrap().to_str().unwrap().to_owned();
    assert!(
        name.starts_with("crash-") && name.ends_with("Z.log"),
        "{name}"
    );
    let text = std::fs::read_to_string(&logs[0]).unwrap();
    assert!(text.contains("the meeting list ran out of rows"), "{text}");
    // `tests\crash_log.rs` on Windows.
    assert!(text.contains("crash_log.rs:"), "{text}");
    assert!(text.contains("on thread 'steno-crash-test'"), "{text}");
    assert!(text.contains("backtrace:"), "{text}");
    assert_eq!(
        previous_ran.load(Ordering::SeqCst),
        1,
        "the expected panic ran no hook"
    );
}
