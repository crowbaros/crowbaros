//! The probe's apps, which the desktop check drives in Base: copy in one app
//! and paste in another, the primary selection with it, and drag from one
//! app and drop on another, as the person would (ADR 0032).

mod common;

use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use common::{Compositor, OUTPUT_SIZE};
use crowbar_probe::apps::{App, Role};

const BTN_LEFT: u32 = 0x110;
const KEY_LEFTMETA: u32 = 125;
const KEY_LEFT: u32 = 105;
const KEY_RIGHT: u32 = 106;

/// One of the probe's apps, running on a thread of its own as it runs in a
/// process of its own in Base, and the lines it says.
struct Running {
    lines: mpsc::Receiver<String>,
}

impl Running {
    /// Maps the app's window, then runs it until the compositor stops.
    fn start(compositor: &Compositor, role: Role) -> Self {
        let mut app = App::open(compositor.app(), role).unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = app.run(&mut |line| {
                let _ = tx.send(line.to_owned());
            });
        });
        Running { lines }
    }

    /// Waits for the app to say `line`, and fails if it says another first.
    fn says(&self, line: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let left = deadline.saturating_duration_since(Instant::now());
        match self.lines.recv_timeout(left) {
            Ok(said) => assert_eq!(said, line),
            Err(_) => panic!("the app never said {line:?}"),
        }
    }

    /// Whether the app says nothing more for a while.
    fn is_quiet(&self) -> bool {
        self.lines.recv_timeout(Duration::from_millis(300)).is_err()
    }
}

/// The middle of the output.
fn middle() -> (i32, i32) {
    (OUTPUT_SIZE.0 / 2, OUTPUT_SIZE.1 / 2)
}

/// Super with an arrow key: snaps the window with the keyboard.
fn super_with(compositor: &Compositor, key: u32) {
    compositor.key(KEY_LEFTMETA, true);
    compositor.key(key, true);
    compositor.key(key, false);
    compositor.key(KEY_LEFTMETA, false);
}

#[test]
fn text_copied_in_one_app_is_pasted_in_another_with_the_primary_selection() {
    let compositor = Compositor::start();

    // Each window takes the keyboard as it opens.
    let copier = Running::start(&compositor, Role::Copy("copied words".into()));
    copier.says("focused");
    copier.says("copied");

    // The paster's window opens above the copier's and takes the keyboard.
    let paster = Running::start(&compositor, Role::Paste);
    paster.says("focused");
    paster.says("pasted clipboard=copied words");
    paster.says("pasted primary=copied words");
    assert!(copier.is_quiet(), "the copier heard something");
}

#[test]
fn each_window_is_maximized_and_says_when_it_gains_the_keyboard_again() {
    let compositor = Compositor::start();
    let (x, y) = middle();
    let copier = Running::start(&compositor, Role::Copy("again".into()));
    copier.says("focused");
    copier.says("copied");
    let paster = Running::start(&compositor, Role::Paste);
    let classes = compositor.with(|state| {
        state
            .window_requests()
            .into_iter()
            .map(|request| request.app_id)
            .collect::<Vec<_>>()
    });
    assert_eq!(classes, ["org.crowbaros.ProbeCopy", "org.crowbaros.ProbePaste"]);
    let maximized = compositor.with(|state| {
        state
            .window_requests()
            .into_iter()
            .map(|request| request.maximized)
            .collect::<Vec<_>>()
    });
    assert_eq!(maximized, [true, true]);
    // Each fills the output, which has no shell to take room from it.
    let rects = compositor.with(|state| state.window_rects());
    for rect in rects {
        assert!(
            rect.width >= OUTPUT_SIZE.0 - 16 && rect.height >= OUTPUT_SIZE.1 - 16,
            "a window does not fill the output: {rect:?}"
        );
    }

    // Each new window took the keyboard as it opened: the copier copied,
    // and the paster, which opened above it, pasted what it copied.
    paster.says("focused");
    paster.says("pasted clipboard=again");
    paster.says("pasted primary=again");

    // The paster covers the copier. A click on it, which has the keyboard
    // already, gives nothing again.
    compositor.click(x, y);
    assert!(paster.is_quiet(), "the paster said something again");
    assert!(copier.is_quiet(), "the copier heard a click on the paster's window");
}

#[test]
fn text_dragged_from_one_app_is_dropped_on_another() {
    let compositor = Compositor::start();
    let (x, y) = middle();
    let (width, _) = OUTPUT_SIZE;

    let dropper = Running::start(&compositor, Role::Drop);
    compositor.click(x, y);
    dropper.says("focused");
    super_with(&compositor, KEY_LEFT);

    let dragger = Running::start(&compositor, Role::Drag("dragged words".into()));
    // A click on it is a press, so it starts a drag that the release ends
    // with nowhere to drop.
    compositor.click(x, y);
    dragger.says("focused");
    dragger.says("drag started");
    dragger.says("drag cancelled");
    super_with(&compositor, KEY_RIGHT);

    // Press on the dragger, in the right half, and carry it to the dropper.
    compositor.pointer(width * 3 / 4, y);
    compositor.with(|state| state.pointer_button(BTN_LEFT, true, 0));
    dragger.says("drag started");
    compositor.pointer(width / 4, y);
    // The dropper takes the offer on its own thread.
    std::thread::sleep(Duration::from_millis(300));
    compositor.pointer(width / 4 + 4, y);
    std::thread::sleep(Duration::from_millis(300));
    compositor.with(|state| state.pointer_button(BTN_LEFT, false, 0));

    dropper.says("dropped dragged words");
    dragger.says("drag finished");
}
