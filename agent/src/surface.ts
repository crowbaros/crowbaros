// The surface socket: where the shell reaches the Harness. README.md has its
// framing and its messages.
import type { Channel } from "./channel.ts";

/**
 * The surface socket's rules, for the shell's user ID. It has no message
 * yet: the slice that builds the Agent's surface gives it the conversation
 * messages the person socket speaks. Until then a frame after the version is
 * one the Harness does not speak, and it closes the connection.
 */
export function surface(shellUid: number): Channel {
	return {
		version: 1,
		uid: shellUid,
		peer: "the shell",
		whose: "the shell's",
		// The longest frame the Harness takes from the shell.
		maxFrame: 4096,
		serve: (connection) => ({
			frame: () => connection.close("the surface socket has no message yet"),
			left: () => {},
		}),
	};
}
