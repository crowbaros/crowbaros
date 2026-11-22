// The surface socket: where the shell reaches the Harness. README.md has its
// framing and its messages.
import { createServer, type Server, type Socket } from "node:net";
import { encode, FrameReader, FrameTooLong } from "./frames.ts";
import { peerUid } from "./peer.ts";

/** The version of the surface socket's protocol. */
export const VERSION = 1;
/** The longest frame the Harness takes from the shell, not counting its length. */
export const MAX_FRAME = 4096;

/** Whether a frame states the surface socket's version and nothing else. */
function statesVersion(frame: Buffer): boolean {
	let message: unknown;
	try {
		message = JSON.parse(frame.toString());
	} catch {
		return false;
	}
	if (typeof message !== "object" || message === null || Array.isArray(message)) return false;
	const fields = Object.entries(message);
	return fields.length === 1 && fields[0]?.[0] === "version" && fields[0][1] === VERSION;
}

/** Serves one connection, and says what became of it for the log. */
function serve(peer: Socket, shellUid: number, say: (what: string) => void): void {
	peer.on("error", () => peer.destroy());
	// Only the shell is served, by the user ID the kernel gives for the peer
	// (ADR 0032). A peer the kernel says nothing of is refused with the rest.
	const uid = peerUid(peer);
	if (uid !== shellUid) {
		say(uid === undefined ? "refused a peer whose user ID the kernel did not give" : `refused user ID ${uid}, which is not the shell's`);
		peer.destroy();
		return;
	}
	say("the shell connected");
	const frames = new FrameReader(MAX_FRAME);
	let stated = false;
	let closed = false;
	// What was already written is sent, and nothing more is read. A peer
	// that does not take it within a second is dropped all the same.
	const close = (why: string) => {
		closed = true;
		say(`closed, ${why}`);
		peer.removeAllListeners("data");
		peer.end(() => peer.destroy());
		setTimeout(() => peer.destroy(), 1000).unref();
	};
	peer.on("close", () => {
		if (!closed) say(stated ? "the shell left" : "the shell left before it stated a version");
	});
	peer.on("data", (bytes: Buffer) => {
		let read: Buffer[];
		try {
			read = frames.read(bytes);
		} catch (error) {
			return close(error instanceof FrameTooLong ? error.message : "a frame could not be read");
		}
		for (const frame of read) {
			if (!stated) {
				if (!statesVersion(frame)) return close(`the shell's first frame did not state version ${VERSION}`);
				stated = true;
				say(`the shell stated version ${VERSION}`);
				peer.write(encode({ version: VERSION }));
				continue;
			}
			// No slice has given the surface socket a message yet, so any
			// frame after the version is one the Harness does not speak.
			return close("the surface socket has no message yet");
		}
	});
}

/**
 * Listens on the surface socket at `path`, for the shell's user ID alone.
 * `say` is told what became of each connection, and nothing a peer sent.
 */
export function listen(path: string, shellUid: number, say: (what: string) => void): Promise<Server> {
	return new Promise((resolve, reject) => {
		const server = createServer((peer) => serve(peer, shellUid, say));
		server.once("error", reject);
		server.listen(path, () => resolve(server));
	});
}
