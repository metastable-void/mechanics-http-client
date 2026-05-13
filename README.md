# mechanics-http-client

Small reqwest-shaped HTTP client built on `hyper-rustls` +
`webpki-roots`, with `aws-lc-rs` as the sole crypto provider.

Owned by the mechanics family. **Does not depend on any
`philharmonic-*` crate.** Philharmonic-family consumers depend
on this crate; the reverse direction is forbidden by the
workspace's Mechanics-Philharmonic independence rule.

## Status

**v0.0.1 — initial scaffolding.** API surface is being built
out in lock-step with the migration of the workspace's reqwest
call sites; expect the public shape to settle and stabilise
through the early v0.0.x releases.

## Why

A general-purpose convenience HTTP client (reqwest) brought
`rustls-platform-verifier` + `rustls-native-certs` into the
workspace's runtime dep tree by way of its `rustls` feature.
The workspace's locked TLS posture is **bundled Mozilla CA bundle
(webpki-roots) only — no native-roots**; rebuilding the necessary
convenience surface on top of `hyper-rustls` directly lets us
own that posture cleanly. As a side benefit, the workspace's
four HTTP-outbound call sites stop duplicating
client-builder + error-classification + body-reading code:
they centralise here.

## License

Dual-licensed under `Apache-2.0 OR MPL-2.0`.

## Contributing

Developed as part of the
[mechanics-rs](https://github.com/metastable-void/mechanics-rs)
family, under the
[philharmonic-workspace](https://github.com/metastable-void/philharmonic-workspace)
parent. Workspace-wide development conventions — git workflow,
script wrappers, Rust code rules, versioning, terminology —
live in the meta-repo, authoritatively in its
[`CONTRIBUTING.md`](https://github.com/metastable-void/philharmonic-workspace/blob/main/CONTRIBUTING.md).

SPDX-License-Identifier: Apache-2.0 OR MPL-2.0
