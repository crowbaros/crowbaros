// Who is at the other end of a Unix socket. Node.js offers no way to ask, so
// this asks the kernel through `node:ffi`, which calls the C library's
// `getsockopt` in the process itself. Nothing is loaded and no native addon
// is built. Linux on x86-64 alone: the option's numbers and the layout of
// `struct ucred` are that platform's.
import { dlopen } from "node:ffi";
import type { Socket } from "node:net";

const SOL_SOCKET = 1;
const SO_PEERCRED = 17;
/** `struct ucred`: the peer's process, user, and group IDs, four bytes each. */
const UCRED_SIZE = 12;

const libc = dlopen(null, {
	getsockopt: { arguments: ["int32", "int32", "int32", "buffer", "buffer"], return: "int32" },
});

/**
 * The user ID of the process that connected `socket`, as the kernel recorded
 * it when the connection was made, or undefined when the kernel would not
 * say. A caller that gets undefined must refuse the peer.
 */
export function peerUid(socket: Socket): number | undefined {
	// The descriptor is Node's own to hold; only its number is read.
	const fd: unknown = (socket as unknown as { _handle?: { fd?: unknown } })._handle?.fd;
	if (typeof fd !== "number" || !Number.isInteger(fd) || fd < 0) return undefined;
	const credentials = Buffer.alloc(UCRED_SIZE);
	const length = Buffer.alloc(4);
	length.writeUInt32LE(UCRED_SIZE);
	if (libc.functions.getsockopt?.(fd, SOL_SOCKET, SO_PEERCRED, credentials, length) !== 0) return undefined;
	if (length.readUInt32LE() !== UCRED_SIZE) return undefined;
	return credentials.readUInt32LE(4);
}
