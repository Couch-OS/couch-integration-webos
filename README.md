# CouchOS LG webOS TV Integration

A native CouchOS integration for LG webOS televisions. It connects directly to the TV over its local webOS control interface and provides remote control, media playback, volume and mute, app access, sound-output selection, and HDMI-CEC input selection.

The integration uses secure webOS pairing. When you first connect, approve Couch on the television; CouchOS securely manages the resulting device credentials.

## Features

- **Remote control** — Navigation, Back, Home, Menu, color buttons, channel controls, and playback controls.
- **Power** — Turn the TV off. Power-on is handled by the CouchOS TV integration host and its configured wake/IR facilities.
- **Volume and mute** — Volume up/down, mute toggle, and explicit mute on/off.
- **Status** — Report volume and mute state while the integration is connected and processing TV responses.
- **Apps** — Discover installed TV apps and launch them.
- **Inputs** — Select the TV's physical inputs and supported downstream HDMI-CEC devices.
- **Sound output** — Choose between the TV's internal speakers and HDMI ARC/eARC output.

### HDMI-CEC inputs

The input selector includes the TV's physical inputs and, when the TV reports them, devices connected through HDMI-CEC. CEC child entries are labelled with their parent input's friendly name (for example, `Soundbar/Receiver - Apple TV`). Selecting a child launches it through the corresponding HDMI input.

CEC discovery depends on information provided by the TV. If the TV does not report a downstream device, that device will not appear in the selector.

### Audio status

The integration subscribes to webOS for mute and volume updates. These notifications are processed while the integration is communicating with the TV. They are not delivered continuously while the integration is idle, so status may not reflect a change until the next operation.

## Requirements

- A CouchOS remote with support for integration package repositories.
- An LG television running webOS with network control enabled.
- The remote and TV must be able to communicate over your local network.
- The TV's IP address. A DHCP reservation for the TV is recommended so its address does not change.

The integration uses the TV's secure webOS control connection on TCP port **3001**. Enter the TV's IP address during setup; the integration configures the secure connection automatically.

## Installation

### 1. Add the LG webOS package repository

In CouchOS, open **Integrations** and choose **Add a custom repository**. Enter these values:

| Field | Value |
| --- | --- |
| Repository ID | `webos` |
| Name | `LG webOS TV Integration` |
| Base URL | `https://joemacjr92.github.io/couch-integration-webos/preview` |
| Public signing key | [webos-repository-public.pem](keys/webos-repository-public.pem) |

The public signing key is also available as a [raw PEM file](https://raw.githubusercontent.com/joemacjr92/couch-integration-webos/main/keys/webos-repository-public.pem). Only the public key is published in this repository. The corresponding private signing key is kept private by the repository maintainer.

### 2. Verify and trust the signing key

1. Copy the contents of the public PEM file into CouchOS.
2. Select **Check public-key fingerprint**.
3. Confirm that the displayed SHA-256 fingerprint matches:

   `499b0a9cfa8de88136f1f608221543eadfb43bea86af2782ed940013a11b332b`

4. If the fingerprint matches, select **Trust repository**.
5. Select **Refresh packages**.

Always compare the fingerprint before trusting the repository. The fingerprint is published here separately so you can verify that the key entered into CouchOS is the expected repository key.

### 3. Remove the built-in package and install this package

If the native LG webOS integration package is already installed on your remote, remove that existing package first. The native package must be removed for this repository's LG webOS package to appear as an available package to choose.

After removing the native package:

1. Open **Integrations** and refresh the package list if necessary.
2. Find **LG webOS TV Integration** in the available packages.
3. Select it and install it.

Removing the native package does not remove or reset your LG TV. It only makes the repository-provided integration package available for installation.

## Configuration and pairing

1. Make sure the TV is powered on and connected to your network.
2. After installing the package, open the CouchOS **Connections** area.
3. Add or configure the **LG webOS TV** integration.
4. Enter the TV's IP address.
5. Start pairing.
6. When prompted, look at the TV and approve the connection from Couch.
7. Complete the pairing flow in CouchOS.

The TV may display a prompt asking whether to allow the connection. Pairing cannot complete until you approve it on the television.

If pairing fails, confirm that the TV is powered on, the IP address is correct, and the remote can reach the TV over the local network. If the TV's pairing identity or certificate has changed, pair the integration again.

## Security

- Communication with the TV uses its secure webOS control endpoint.
- Pairing requires approval on the TV.
- The client key and pinned TV certificate are handled as CouchOS credentials, not ordinary integration settings.
- The repository's public signing key is provided so CouchOS can verify packages from this feed.
- The matching private repository signing key is not included in this repository.

Only trust the repository after verifying its public-key fingerprint.

## Compatibility and notes

This integration targets LG webOS televisions. Available apps, input details, HDMI-CEC information, sound outputs, and individual remote functions depend on the TV model and webOS version.

The package uses CouchOS's native TV host and integration framework. Network power-on is not provided by this package; power-on behavior remains the responsibility of the host's configured wake or infrared support.

## Credits

- **CouchOS** — Integration framework and remote platform.
- **LG** — webOS television platform and SSAP control interface.

## License

This project is licensed under the GNU General Public License v3.0 or later. See [LICENSE](LICENSE) for details.
