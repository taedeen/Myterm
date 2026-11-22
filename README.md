# MyTerm

A multi-platform SSH management tool: connect to many hosts, with autocomplete, an encrypted credential vault, shortcut keys (including a secure **Type password** key), end-to-end encrypted sync, and hardware-bound keys.



**Using MyTerm:** see the [user guide](docs/user-guide.md): deploying the sync server, installing the apps, and everyday use.

#The phone app needs Android 11 (API 30) or later, for per-use Keystore authentication.

## Security notes

- Hosts, credentials, snippets and history are vault records. Everything is encrypted on the device, and the sync payload is ciphertext.
- Secrets never cross into Dart after they're stored. Type password writes them from Rust straight to the SSH channel.
- Hardware keys are generated in StrongBox where available, otherwise the TEE. Every signature needs biometrics or the device PIN. Keys are invalidated when biometrics change, and they never sync.
- The vault master key is wrapped twice: with Argon2id from the master password, and with a Keystore AES key that needs biometrics on every use.

## Status

Version 0.5.0 (see [CHANGELOG.md](CHANGELOG.md)).

