# The desktop's helpers

The four small programs at the desktop's security boundaries, in one Rust
binary, `crowbar-helpers`. Base installs it in `/usr/local/bin` with a link
for each helper, and the binary takes the helper from the name it runs by:

| Command | Runs as | What it does |
| --- | --- | --- |
| `crowbar-unlock` | root | The compositor's unlock authority ([ADR 0032](../../docs/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md)). It takes the person's password from the shell's user ID alone, checks it against `/etc/shadow` through libcrypt, and only then tells the compositor to unlock. A wrong password costs a pause, and it hears one attempt at a time. `crowbar-unlock lock` locks, as zzz's suspend hook does; `crowbar-unlock hash PASSWORD` prints a hash for `/etc/shadow`, as the image build sets the lab's password |
| `crowbar-launcher` | root, then the person | Binds the launcher's socket, becomes the person's account with its groups, and starts the desktop entries the shell asks for with `gio launch`. Reads the person's saved appearance for the shell. Takes requests from the shell's user ID alone |
| `crowbar-shell-connect` | the shell's account | Connects the shell to the compositor's sockets, checks the compositor's account serves them, and becomes the shell with both connections |
| `crowbar-ctrl-alt-del` | root | Takes `ctrl-alt-del` from the compositor's account alone, and sends init the SIGINT the kernel would |

`crowbar-helpers <helper> [ARGS]` runs a helper by name.

Each checks its peer with `SO_PEERCRED`. Their runit services are in
[`../desktop/`](../desktop/).

On its own socket a helper takes one line of JSON and answers with one. To
the compositor, `crowbar-unlock` speaks the compositor's framing
(`src/frames.rs`): a length and then JSON, with the version stated first, as
[the control channel](../../compositor/README.md#the-control-channel) has
it.

## Build and test

Base's build builds and tests it in its Void build root with the compositor
and the apps. On a host with Rust and libxcrypt:

```sh
cargo test --locked
```

The tests need no root, and serve real sockets in the system's temporary
directory.
