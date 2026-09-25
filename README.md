# Couch LG webOS integration

This repository is the extraction candidate for Couch's built-in LG webOS
client. It owns the TV's SSAP WebSocket, on-screen approval pairing, client key
and pinned certificate, navigation, playback, volume, mute, status and input
selection. Couch keeps the native screen and the integration host.

`0.1.0_pre5` restores the native Apps, Picture and Sound output cards. It uses
protocol 5 to enumerate installed launch points, launch an app, report the
current sound output and expose Couch-owned sound choices. The Picture card
opens the TV's own Settings app, matching the original built-in screen.

`0.1.0_pre4` accepts the resource path in the pointer WebSocket URL returned by
the TV, restoring D-pad and other pointer-backed controls. The setup field
remains restricted to the TV's IP address or a root WebSocket URL.

`0.1.0_pre3` also reports the foreground app's human name when the TV includes
it in status. A core containing the packaged-TV touchscreen update recognizes this
package's existing input selector and complete standard TV capability set,
restores Couch's TV hero and transport row, and routes the physical D-pad to
the television. That release remained compatible with protocol-3 cores, which
kept the generic package screen until the core was updated. `0.1.0_pre5`
requires protocol 5 because app discovery is a new host/package exchange.

The package is intentionally preview-only. Network power-on remains outside the
current package boundary because it needs a host-learned Wake-on-LAN address or
the remote's privileged infrared device. This package therefore offers network
power-off only.

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
