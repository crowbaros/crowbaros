// What every socket the Harness listens on shares: one user ID served and
// every other peer refused, the framing, and the version stated first.
// README.md has the framing. What a socket's peer may ask once it has stated
// the version is that socket's own: surface.ts and person.ts.
import { createServer, type Server, type Socket } from "node:net";
import { encode, FrameReader, FrameTooLong } from "./frames.ts";
import { peerUid } from "./peer.ts";

/**
 * How much may wait to be read by a peer before it is closed: a peer that
 * reads nothing must not have the Harness keep an answer for it without end.
 */
const MAX_WAITING = 16 * 1024 * 1024;

/** One socket's rules. */
export interface Channel {
	/** The version of this socket's protocol. */
	version: number;
	/** The one user ID served. */
	uid: number;
	/** The peer, for the log: "the shell". */
	peer: string;
	/** Whose user ID that is, for the log: "the shell's". */
	whose: string;
	/** The longest frame taken from the peer, not counting its length. */
	maxFrame: number;
	/**
	 * Takes a peer that has stated the version, and gives what is done with
	 * each frame it sends after that, and what is done when it leaves.
	 */
	serve(connection: Connection): { frame(frame: Buffer): void; left(): void };
}

/** A peer that has stated the version. */
export interface Connection {
	/** Sends one message, unless the peer has gone or been closed. */
	send(message: unknown): void;
	/** Sends what was already written and reads no more. `why` goes to the log. */
	close(why: string): void;
}

/** Whether a frame states this version and nothing else. */
function statesVersion(frame: Buffer, version: number): boolean {
	let message: unknown;
	try {
		message = JSON.parse(frame.toString());
	} catch {
		return false;
	}
	if (typeof message !== "object" || message === null || Array.isArray(message)) return false;
	const fields = Object.entries(message);
	return fields.length === 1 && fields[0]?.[0] === "version" && fields[0][1] === version;
}

/** Serves one connection, and says what became of it for the log. */
function serve(peer: Socket, channel: Channel, say: (what: string) => void): void {
	peer.on("error", () => peer.destroy());
	// One user ID is served, by the one the kernel gives for the peer (ADR
	// 0032). A peer the kernel says nothing of is refused with the rest.
	const uid = peerUid(peer);
	if (uid !== channel.uid) {
		say(uid === undefined ? "refused a peer whose user ID the kernel did not give" : `refused user ID ${uid}, which is not ${channel.whose}`);
		peer.destroy();
		return;
	}
	say(`${channel.peer} connected`);
	const frames = new FrameReader(channel.maxFrame);
	let served: ReturnType<Channel["serve"]> | undefined;
	let closed = false;
	// What was already written is sent, and nothing more is read. A peer
	// that does not take it within a second is dropped all the same.
	const close = (why: string) => {
		if (closed) return;
		closed = true;
		say(`closed, ${why}`);
		peer.removeAllListeners("data");
		peer.end(() => peer.destroy());
		setTimeout(() => peer.destroy(), 1000).unref();
	};
	const connection: Connection = {
		send(message) {
			if (closed || peer.destroyed) return;
			peer.write(encode(message));
			if (peer.writableLength > MAX_WAITING) close(`${channel.peer} read nothing while ${MAX_WAITING} bytes waited for it`);
		},
		close,
	};
	peer.on("close", () => {
		if (!closed) say(served ? `${channel.peer} left` : `${channel.peer} left before it stated a version`);
		closed = true;
		served?.left();
	});
	peer.on("data", (bytes: Buffer) => {
		try {
			for (const frame of frames.read(bytes)) {
				if (closed) return;
				if (served === undefined) {
					if (!statesVersion(frame, channel.version)) return close(`${channel.peer}'s first frame did not state version ${channel.version}`);
					say(`${channel.peer} stated version ${channel.version}`);
					peer.write(encode({ version: channel.version }));
					served = channel.serve(connection);
					continue;
				}
				served.frame(frame);
			}
		} catch (error) {
			if (!(error instanceof FrameTooLong)) throw error;
			close(error.message);
		}
	});
}

/**
 * Listens at `path` by this channel's rules. `say` is told what became of
 * each connection, and nothing a peer sent.
 */
export function listen(path: string, channel: Channel, say: (what: string) => void): Promise<Server> {
	return new Promise((resolve, reject) => {
		const server = createServer((peer) => serve(peer, channel, say));
		server.once("error", reject);
		// An error once it listens is the Harness's to end on and log, as any
		// other it did not expect.
		server.listen(path, () => {
			server.off("error", reject);
			resolve(server);
		});
	});
}
