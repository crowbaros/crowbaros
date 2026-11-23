//! The line the compositor logs for the checks that run it: commits by
//! window and layer, and frames drawn.

use crate::common;

use common::Compositor;
use crowbar_probe::LayerRequest;
use fulcrum_wire::protocol::wlr_layer_shell_unstable_v1::zwlr_layer_surface_v1::anchor::{LEFT, RIGHT, TOP};

#[test]
fn the_report_counts_commits_by_window_and_layer_and_the_frames_drawn() {
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    shell
        .open_layer(
            LayerRequest {
                namespace: "crowbaros-bar",
                layer: 2,
                anchor: TOP | LEFT | RIGHT,
                size: (0, 44),
                exclusive_zone: 44,
                keyboard: 0,
            },
            0xff20_4060,
        )
        .unwrap();
    let mut client = compositor.app();
    let window = client
        .open_toplevel("app", "com.crowbaros.StudioSettings", (100, 60), 0xffff_ffff)
        .unwrap();
    client.redraw(&window, (100, 60), 0xff00_0000).unwrap();
    compositor.wait_until_still();

    let report = compositor.with(|state| state.commit_report());

    // The spike's form, which CrowbarOS's desktop check reads.
    assert!(report.starts_with("commits so far: "), "{report}");
    assert!(report.contains("layer crowbaros-bar: 2"), "{report}");
    // From the commit that mapped it.
    assert!(report.contains(" com.crowbaros.StudioSettings: 2"), "{report}");
    let frames: u64 = report.rsplit("frames=").next().unwrap().parse().unwrap();
    assert!(frames >= 1, "{report}");
}

/// After the workspaces change, the time until each layer draws is kept and
/// logged, so the desktop check can tell how soon the bar shows the change.
/// A commit that changes nothing, as for a frame callback alone, is not it.
#[test]
fn the_time_from_a_workspace_change_to_a_layers_next_drawing_is_kept() {
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    let bar = shell
        .open_layer(
            LayerRequest {
                namespace: "crowbaros-bar",
                layer: 2,
                anchor: TOP | LEFT | RIGHT,
                size: (0, 44),
                exclusive_zone: 44,
                keyboard: 0,
            },
            0xff20_4060,
        )
        .unwrap();
    compositor.wait_until_still();
    assert!(
        compositor
            .with(|state| state.drawn_since_workspaces_changed())
            .is_empty()
    );

    compositor.with(|state| state.show_workspace(2));
    shell.commit_unchanged(bar.surface).unwrap();
    shell.roundtrip().unwrap();
    assert!(
        compositor
            .with(|state| state.drawn_since_workspaces_changed())
            .is_empty(),
        "a commit that changed nothing counted"
    );

    let size = (bar.size.0 as i32, bar.size.1 as i32);
    let buffer = shell.shm_buffer(size, 0xff40_8020).unwrap();
    shell.show(bar.surface, Some(&buffer)).unwrap();
    shell.roundtrip().unwrap();
    shell.show(bar.surface, Some(&buffer)).unwrap();
    shell.roundtrip().unwrap();

    let drawn = compositor.with(|state| state.drawn_since_workspaces_changed());
    assert_eq!(drawn.len(), 1, "once for the change: {drawn:?}");
    assert_eq!(drawn[0].0, "layer crowbaros-bar");
    assert!(drawn[0].1 < std::time::Duration::from_secs(5), "{drawn:?}");
}
