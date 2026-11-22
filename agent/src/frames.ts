// The framing every channel of the Agent speaks, which ADR 0035 gave
// Fulcrum's channels and the Milestone 3 spec gives the rest: a message is a
// frame, its length in four bytes, the most significant first, and then that
// many bytes of JSON. This file holds the framing alone, and no socket.

/** One message as a frame. */
export function encode(message: unknown): Buffer {
	const json = Buffer.from(JSON.stringify(message));
	const frame = Buffer.alloc(4 + json.length);
	frame.writeUInt32BE(json.length);
	json.copy(frame, 4);
	return frame;
}

/** A frame whose length says more than the reader takes. */
export class FrameTooLong extends Error {
	constructor(length: number, limit: number) {
		super(`a frame of ${length} bytes is longer than the ${limit} taken`);
	}
}

/** Turns the bytes a peer sends, in whatever pieces they come, into frames. */
export class FrameReader {
	#limit: number;
	#waiting: Buffer = Buffer.alloc(0);

	constructor(limit: number) {
		this.#limit = limit;
	}

	/**
	 * Each frame these bytes complete, without its length. Throws
	 * `FrameTooLong` as soon as a length over the limit is read, before any of
	 * that frame is kept.
	 */
	read(bytes: Buffer): Buffer[] {
		this.#waiting = Buffer.concat([this.#waiting, bytes]);
		const frames: Buffer[] = [];
		while (this.#waiting.length >= 4) {
			const length = this.#waiting.readUInt32BE(0);
			if (length > this.#limit) throw new FrameTooLong(length, this.#limit);
			if (this.#waiting.length < 4 + length) break;
			frames.push(this.#waiting.subarray(4, 4 + length));
			this.#waiting = this.#waiting.subarray(4 + length);
		}
		return frames;
	}
}
