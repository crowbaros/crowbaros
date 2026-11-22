// Faults that nothing outside the Harness's process can cause, for the tests
// that need one. `launch` in harness.ts loads this file into the process
// ahead of the bundle, and HARNESS_TEST_FAULT names the fault to cause. The
// Harness itself never loads it.
import fs from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import { Server } from "node:net";

const fault = process.env.HARNESS_TEST_FAULT;

/** Runs `then` once the surface socket listens, before the Harness is told that it does. */
function asItListens(then: (server: Server) => void): void {
	const listen = Server.prototype.listen as (this: Server, ...args: unknown[]) => Server;
	Server.prototype.listen = function (this: Server, ...args: unknown[]) {
		const told = args.at(-1);
		if (typeof told !== "function") return listen.apply(this, args);
		return listen.apply(this, [...args.slice(0, -1), () => (then(this), told())]);
	} as typeof Server.prototype.listen;
}

/**
 * The first write to any file takes 20 bytes and no more, as the kernel may
 * when a disk is nearly full. With `thenFails`, the write after it fails, as
 * it does once the disk is full.
 */
function writeInPart(thenFails: boolean): void {
	const writeSync = fs.writeSync as (fd: number, data: Uint8Array, offset?: number, length?: number) => number;
	let writes = 0;
	fs.writeSync = ((fd: number, data: Uint8Array, offset: number = 0) => {
		writes++;
		if (writes === 1) return writeSync(fd, data, offset, 20);
		if (writes === 2 && thenFails) throw new Error("ENOSPC: no space left on device, write");
		return writeSync(fd, data, offset);
	}) as typeof fs.writeSync;
	// The bundle imports `writeSync` by name, and this gives that name the new one.
	syncBuiltinESMExports();
}

/** Runs `then` once the Harness has left its `running` marker, which it does before it listens. */
function onceItIsMarked(then: () => void): void {
	const writeFileSync = fs.writeFileSync;
	fs.writeFileSync = ((...args: Parameters<typeof fs.writeFileSync>) => {
		writeFileSync(...args);
		if (String(args[0]).endsWith("/running")) then();
	}) as typeof fs.writeFileSync;
	syncBuiltinESMExports();
}

switch (fault) {
	case "stopped before it listens":
		onceItIsMarked(() => process.kill(process.pid, "SIGTERM"));
		break;
	case "stopped as it listens":
		asItListens(() => process.kill(process.pid, "SIGTERM"));
		break;
	case "the listening socket fails":
		asItListens((server) => setImmediate(() => server.emit("error", new Error("the listening socket failed"))));
		break;
	case "a write is taken in part":
		writeInPart(false);
		break;
	case "a write is taken in part, and the next fails":
		writeInPart(true);
		break;
	default:
		throw new Error(`no such fault: ${fault}`);
}
