# platform-sdk

The shared platform contract of the Roses project's multi-game launcher:
used by the launcher, the platform account service, and every game (its
client, importer and online server). It contains no game code and no game
data.

- `manifest`: the per-game manifest the launcher's registry reads (id,
  names, required ISO fingerprints, importer, executable, online server,
  mod support).
- `contract`: how the launcher starts a game's importer and executable
  (arguments, environment variables, the session hand-off file).
- `mods`: the mod manifest format and load-order resolution.
- `token`: platform access tokens, Ed25519-signed by the account service
  and verified offline by game servers.

`crates/platform-net`, the network layer shared by the launcher and the
games:

- `transport`, `tls`: framed JSON connections, TLS with the web PKI or a
  pinned self-signed certificate.
- `config`: the server file (`online.json`), the saved session
  (`online-session.json`), connecting.
- `account`: the account protocol (sign-up, log-in, sessions, password
  change, email) that every game lobby carries, and an account-only client.
- `profile`: the launcher Profile page's jobs over it.

```powershell
cargo build --workspace
cargo test --workspace
```

This repository depends on nothing else of the project; the others use it
through sibling checkouts, see [docs/REPOSITORIES.md](docs/REPOSITORIES.md).

## Licence

Dual-licensed under either of

- the MIT licence ([LICENSE-MIT](LICENSE-MIT)), or
- the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)),

at your option. Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in the work by you, as defined in the
Apache-2.0 licence, shall be dual-licensed as above, without any additional
terms or conditions.
