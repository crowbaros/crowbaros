// The Harness's log: one line for each start, restart, connection, stop, and
// error, in a file the person alone can read. It never holds what a
// conversation says or what an act printed: callers pass it events and the
// IDs, counts, and paths that go with them, and nothing they were sent.
import { closeSync, fchmodSync, fstatSync, openSync, renameSync, writeSync } from "node:fs";

/**
 * A line that would take the log past this size is written to a new log, and
 * the old one is set aside as `<log>.1`, in place of the one set aside before
 * it. So the two together stay within twice the size, whether a Harness that
 * cannot start is started again each second and logs each time, or one that
 * runs is connected to without end.
 */
const MAX_LOG = 4 * 1024 * 1024;

export type Event = "start" | "restart" | "listening" | "connection" | "stop" | "error";

export class Log {
	#path: string;
	#file: number;
	#size: number;

	/** Opens the log at `path` for appending, the person's alone whatever the process's mask. */
	constructor(path: string) {
		this.#path = path;
		this.#file = this.#open();
		this.#size = fstatSync(this.#file).size;
	}

	#open(): number {
		const file = openSync(this.#path, "a", 0o600);
		fchmodSync(file, 0o600);
		return file;
	}

	/** One line: the time in UTC, the event, and its details. Written before this returns. */
	write(event: Event, details: string): void {
		const line = Buffer.from(`${new Date().toISOString()} ${event}: ${details.replaceAll("\n", "\n    ")}\n`);
		if (this.#size > 0 && this.#size + line.length > MAX_LOG) {
			renameSync(this.#path, `${this.#path}.1`);
			closeSync(this.#file);
			this.#file = this.#open();
			this.#size = 0;
		}
		writeSync(this.#file, line);
		this.#size += line.length;
	}

	/** An error, with where it was thrown. */
	error(error: unknown): void {
		this.write("error", error instanceof Error ? (error.stack ?? error.message) : String(error));
	}
}
