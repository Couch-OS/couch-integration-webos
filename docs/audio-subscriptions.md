# Synchronous webOS audio subscriptions

## Purpose

The integration subscribes to webOS audio status and volume so it can keep a
small, connection-scoped cache of the latest mute and volume values observed
while the synchronous SSAP connection is being used.

This is not a background receiver. The Couch client SDK contract is
synchronous, and this integration performs socket I/O on the calling thread.
Subscription notifications are processed only while an ordinary operation is
already reading from the SSAP WebSocket. No notifications are received while
the package is idle, and the integration cannot push state directly to Couch.

## Subscriptions

During connection setup, the integration makes two best-effort subscriptions:

- `ssap://audio/getStatus`: the `mute` boolean is the sole source of
  subscription-derived mute state.
- `ssap://audio/getVolume`: volume is read from
  `volumeStatus.volume`, with a legacy top-level `volume` fallback.

Each subscription has its own SSAP message ID. The IDs remain registered for
the lifetime of the `WebOsTv` connection. While waiting for an ordinary
request, the receive loop routes frames matching those IDs into the audio
state cache and continues waiting for the ordinary request's own ID. A
subscription notification cannot complete an unrelated request.

Malformed or unsupported subscription responses do not prevent the ordinary
client from connecting. A broken transport remains a connection failure.
Invalid notification fields leave the last valid cached value unchanged.

## Command and status behavior

- Ordinary `getVolume` reads remain in place. A valid value from the ordinary
  response is preferred; the cached subscription value is a fallback when
  that response does not contain a valid volume.
- The mute toggle first performs an ordinary volume read so queued subscription
  notifications can be processed. It then toggles the most recently observed
  mute state. If no valid mute state has been observed, it does not guess.
- Explicit mute-on and mute-off commands update the cached mute state only
  after the corresponding webOS command succeeds.
- Successful mute commands also update the cache, keeping the next status
  response consistent with the command just issued.
- Cached state is scoped to one connection and is discarded when that client
  is replaced.

## Limits

The cache is opportunistic, not continuously fresh. If the package is idle,
the socket is not being read and new notifications remain queued until a
later synchronous operation reads them. This implementation therefore does
not provide immediate Couch UI updates or an always-on state stream.

A continuously running receiver would require an explicit asynchronous
transport and event-delivery contract in the Couch SDK and package protocol.
It must not be implemented by adding an unapproved worker thread or internal
request queue to this synchronous client.

## Verification

Unit tests cover endpoint-specific payload parsing, explicit false mute
values, malformed-field preservation, subscription acknowledgement handling,
and message-ID routing.

The fake-TV admission test exercises both subscription handshakes and sends
mute and volume notifications before an ordinary command response. This checks
that notifications update cached state without satisfying the pending command.
Physical-device validation has also confirmed mute and volume percentage
reporting on the target TV.
