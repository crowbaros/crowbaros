// The Harness's log: one line for each start, restart, connection, stop, and
// error, in a file the person alone can read. It never holds what a
// conversation says or what an act printed: callers pass it events and the
// IDs, counts, and paths that go with them, and nothing they were sent.
import { fchmodSync, openSync, writeSync } from "node:fs";

export type Event = "start" | "restart" | "listening" | "connection" | "stop" | "error";

export class Log {
	#file: number;

	/** Opens the log at `path` for appending, the person's alone whatever the process's mask. */
	constructor(path: string) {
		this.#file = openSync(path, "a", 0o600);
		fchmodSync(this.#file, 0o600);
	}

	/** One line: the time in UTC, the event, and its details. Written before this returns. */
	write(event: Event, details: string): void {
		writeSync(this.#file, `${new Date().toISOString()} ${event}: ${details.replaceAll("\n", "\n    ")}\n`);
	}

	/** An error, with where it was thrown. */
	error(error: unknown): void {
		this.write("error", error instanceof Error ? (error.stack ?? error.message) : String(error));
	}
}
