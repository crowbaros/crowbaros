// The Harness's log: one line for each start, restart, connection, stop, and
// error, in a file the person alone can read. It never holds what a
// conversation says or what an act printed: callers pass it events and the
// IDs, counts, and paths that go with them, and nothing they were sent.
import { chmodSync, fchmodSync, openSync, renameSync, statSync, writeSync } from "node:fs";

/**
 * A log past this size is set aside when the next Harness starts, as
 * `<log>.1`, in place of the one set aside before it. A Harness that cannot
 * start is started again each second and logs each time, so this keeps its
 * log within twice the size.
 */
const MAX_LOG = 4 * 1024 * 1024;

export type Event = "start" | "restart" | "listening" | "connection" | "stop" | "error";

export class Log {
	#file: number;

	/** Opens the log at `path` for appending, the person's alone whatever the process's mask. */
	constructor(path: string) {
		if ((statSync(path, { throwIfNoEntry: false })?.size ?? 0) > MAX_LOG) {
			chmodSync(path, 0o600);
			renameSync(path, `${path}.1`);
		}
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
