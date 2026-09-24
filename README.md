# Couch LG webOS integration

This repository is the extraction candidate for Couch's built-in LG webOS
client. It owns the TV's SSAP WebSocket, on-screen approval pairing, client key
and pinned certificate, navigation, playback, volume, mute, status and input
selection. Couch keeps the native screen and the integration host.

`0.1.0_pre3` also reports the foreground app's human name when the TV includes
it in status. A core containing the packaged-TV touchscreen update recognizes this
package's existing input selector and complete standard TV capability set,
restores Couch's TV hero and transport row, and routes the physical D-pad to
the television. Older protocol-3 cores continue to accept and run the package;
they retain the generic package screen until the core is updated.

The package is intentionally preview-only. The current protocol does not let a
package enumerate or launch apps, and power-on remains a core concern because
the built-in implementation may use the remote's privileged infrared device or
a host-learned Wake-on-LAN address. This package therefore offers network
power-off only. Do not remove the built-in client until those boundaries have
been implemented and the package has passed side-by-side hardware validation.

The only public setting is the TV IP address. The package uses encrypted webOS
control on port 3001 automatically. Full `ws://` and `wss://` URLs saved by the
first preview remain accepted when that package is upgraded. Pairing asks the
owner to approve Couch on the TV. The client key and exact TLS certificate are
returned as a protocol-3 credential; they never enter settings, argv, the
environment or exported Couch configuration.

## Build and test

```sh
cargo fmt -- --check
cargo test --locked --all-targets
cargo build --locked --release --bin couch-plugin-webos
```

No test or build command pairs with a physical TV. Hardware validation must be
scheduled separately and should cover a secure pairing, reconnect, all remote
buttons, status and input selection, network power-off, core-owned wake/IR power,
package upgrade and rollback.
