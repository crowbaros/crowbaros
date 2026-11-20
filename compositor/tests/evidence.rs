//! The line the compositor logs for the checks that run it: commits by
//! window and layer, and frames drawn.

mod common;

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
