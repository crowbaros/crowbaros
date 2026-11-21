//! The layout of displays (ADR 0020): each monitor's mode, place, and
//! scale, which the shell sets over its control channel and the compositor
//! keeps by the monitor's identity.

mod common;

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

use common::{Compositor, own_uid};
use crowbar_compositor::geometry::Rect;

struct Control {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Control {
    fn connect(compositor: &Compositor) -> Control {
        let stream = UnixStream::connect(compositor.dir.join("control")).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        Control {
            reader: BufReader::new(stream.try_clone().unwrap()),
            writer: stream,
        }
    }

    /// Sends one line and reads the reply, passing over the snapshots the
    /// compositor sends unasked.
    fn ask(&mut self, request: &str) -> String {
        self.writer.write_all(format!("{request}\n").as_bytes()).unwrap();
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            if !line.starts_with(r#"{"event":"snapshot","#) {
                return line.trim_end().to_string();
            }
        }
    }
}

const OK: &str = r#"{"reply":"ok"}"#;
/// The monitor the tests plug in.
const MONITOR: &str = "CRB Studio 27 SN0042";

fn layout_file(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fulcrum-layout-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("layout")
}

/// A compositor that keeps its layout in `file`, with the monitor plugged
/// in as one output that can show two modes, and a second output beside it.
fn plugged(file: PathBuf) -> Compositor {
    Compositor::start_with(own_uid(), own_uid(), move |state| {
        state.keep_layout_in(&file);
        // As a backend does, the monitor is lit in the mode the layout keeps.
        let size = state
            .saved_mode(MONITOR)
            .map_or((2560, 1440), |(width, height, _)| (width, height));
        let output = state.add_output("DP-1", size, 60_000);
        // 27 inches: at 2560x1440 a scale of 1 suits it.
        state.identify_output(
            output,
            MONITOR,
            vec![(2560, 1440, 60_000), (1920, 1080, 60_000)],
            (597, 336),
        );
        state.add_output("HDMI-A-1", (1920, 1080), 60_000);
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
    })
}

fn rects(compositor: &Compositor) -> Vec<Rect> {
    compositor.with(|state| state.outputs().iter().map(|output| output.rect()).collect())
}

#[test]
fn the_shell_reads_each_output_and_its_modes() {
    let compositor = plugged(layout_file("read"));
    let mut control = Control::connect(&compositor);

    let reply = control.ask(r#"{"op":"outputs"}"#);

    assert!(reply.starts_with(r#"{"reply":"outputs","outputs":[{"name":"DP-1","identity":"CRB Studio 27 SN0042","width":2560,"height":1440,"refresh":60000,"x":0,"y":0,"scale":120,"modes":[{"width":2560,"height":1440,"refresh":60000},{"width":1920,"height":1080,"refresh":60000}]}"#), "{reply}");
    assert!(reply.contains(r#""name":"HDMI-A-1","identity":"HDMI-A-1""#), "{reply}");
}

#[test]
fn the_shell_sets_a_mode_a_place_and_a_scale_and_the_monitor_keeps_them() {
    let file = layout_file("kept");
    {
        let compositor = plugged(file.clone());
        let mut control = Control::connect(&compositor);

        let reply = control.ask(
            r#"{"op":"configure_output","name":"DP-1","width":1920,"height":1080,"refresh":60000,"x":1920,"y":0,"scale":180}"#,
        );
        assert_eq!(reply, OK);
        // 1920x1080 at 1.5 covers 1280x720, to the right of the other.
        let ok = control.ask(r#"{"op":"configure_output","name":"HDMI-A-1","x":0,"y":0}"#);
        assert_eq!(ok, OK);
        assert_eq!(
            rects(&compositor),
            [Rect::new(1920, 0, 1280, 720), Rect::new(0, 0, 1920, 1080)]
        );
    }

    // The compositor starts again: the monitor takes what it was given.
    let compositor = plugged(file);
    let (size, rect) = compositor.with(|state| (state.outputs()[0].size, state.outputs()[0].rect()));
    assert_eq!(rect, Rect::new(1920, 0, 1280, 720));
    assert_eq!(
        compositor.with(|state| state.saved_mode(MONITOR)),
        Some((1920, 1080, 60_000)),
        "the mode, for a backend to light the monitor in"
    );
    assert_eq!(size, (1920, 1080));
}

#[test]
fn what_the_layout_cannot_take_is_refused_and_changes_nothing() {
    let compositor = plugged(layout_file("refused"));
    let mut control = Control::connect(&compositor);
    let before = rects(&compositor);

    for request in [
        r#"{"op":"configure_output","name":"DP-2","scale":120}"#,
        r#"{"op":"configure_output","name":"DP-1","scale":135}"#,
        r#"{"op":"configure_output","name":"DP-1","scale":480}"#,
        r#"{"op":"configure_output","name":"DP-1","width":800,"height":600}"#,
        r#"{"op":"configure_output","name":"DP-1","width":1920}"#,
        r#"{"op":"configure_output","name":"DP-1","x":5}"#,
        r#"{"op":"configure_output","name":"DP-1","colour":"blue"}"#,
    ] {
        let reply = control.ask(request);
        assert!(reply.starts_with(r#"{"reply":"error""#), "{request} gave {reply}");
    }
    assert_eq!(rects(&compositor), before);
}

/// A 4K monitor of 27 inches.
const DENSE: &str = "CRB Studio 27 4K SN0043";
const DENSE_MM: (u32, u32) = (597, 336);
const FOUR_K: (i32, i32, i32) = (3840, 2160, 60_000);

/// Plugs a monitor in as a backend does: lit in the mode the layout keeps,
/// or else its own, and then named.
fn plug(
    state: &mut crowbar_compositor::State,
    name: &str,
    identity: &str,
    mode: (i32, i32, i32),
    mm: (u32, u32),
) -> u64 {
    let (width, height, refresh) = state.saved_mode(identity).unwrap_or(mode);
    let output = state.add_output(name, (width, height), refresh);
    state.identify_output(output, identity, vec![mode], mm);
    output
}

/// A compositor that keeps its layout in `file`, with the dense monitor
/// plugged in.
fn dense(file: PathBuf) -> Compositor {
    Compositor::start_with(own_uid(), own_uid(), move |state| {
        state.keep_layout_in(&file);
        plug(state, "DP-1", DENSE, FOUR_K, DENSE_MM);
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
    })
}

fn scales(compositor: &Compositor) -> Vec<u32> {
    compositor.with(|state| state.outputs().iter().map(|output| output.scale_120).collect())
}

#[test]
fn a_dense_monitor_the_layout_does_not_name_starts_at_a_readable_scale() {
    let file = layout_file("dense");
    let compositor = dense(file.clone());

    assert_eq!(scales(&compositor), [180]);
    // 3840x2160 at 1.5 covers 2560x1440.
    assert_eq!(rects(&compositor), [Rect::new(0, 0, 2560, 1440)]);
    assert!(!file.exists(), "a scale the person did not set is not kept");
}

#[test]
fn a_monitor_that_reports_no_size_starts_at_1() {
    let compositor = Compositor::start_with(own_uid(), own_uid(), |state| {
        plug(state, "DP-1", DENSE, FOUR_K, (0, 0));
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
    });

    assert_eq!(scales(&compositor), [120]);
}

#[test]
fn the_scale_the_person_set_wins_over_the_default_after_a_restart() {
    let file = layout_file("chosen");
    {
        let compositor = dense(file.clone());
        let mut control = Control::connect(&compositor);
        assert_eq!(
            control.ask(r#"{"op":"configure_output","name":"DP-1","scale":120}"#),
            OK
        );
        assert_eq!(scales(&compositor), [120]);
    }

    let compositor = dense(file);
    assert_eq!(scales(&compositor), [120]);
}

#[test]
fn a_monitor_plugged_in_again_keeps_the_scale_the_person_set() {
    let compositor = dense(layout_file("replugged"));
    let mut control = Control::connect(&compositor);
    assert_eq!(
        control.ask(r#"{"op":"configure_output","name":"DP-1","scale":210}"#),
        OK
    );

    let scale = compositor.with(|state| {
        let number = state.outputs()[0].number;
        state.remove_output(number);
        // Another port this time.
        let output = plug(state, "HDMI-A-1", DENSE, FOUR_K, DENSE_MM);
        state.outputs().iter().find(|o| o.number == output).unwrap().scale_120
    });
    assert_eq!(scale, 210);
}

#[test]
fn a_monitor_plugged_in_again_without_a_chosen_scale_takes_the_default_again() {
    let compositor = dense(layout_file("replugged-default"));

    let scale = compositor.with(|state| {
        let number = state.outputs()[0].number;
        state.remove_output(number);
        let output = plug(state, "DP-1", DENSE, FOUR_K, DENSE_MM);
        state.outputs().iter().find(|o| o.number == output).unwrap().scale_120
    });
    assert_eq!(scale, 180);
}
