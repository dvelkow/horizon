# Phase 2: browser multiplayer

The browser launcher signs friends in with a shared invite and connects their existing Horizon protocol session through `/game` to a fixed loopback dedicated native server. The authentication secret remains in a secure session cookie; no cloud API credential is part of the application.

[Gateway setup and tests](../web/server/README.md) describe local configuration, TLS, origin checks and the fixed upstream. [Memory measurements](../web/server/deploy/memory-results.md) document the compact gameplay-data decoder and dedicated mesh reduction needed for a small host.

The native engine remains authoritative. Asset caching and OPFS settings persistence are provided by Phase 1. Browser mod execution and the approval portal are not implemented in this branch.

Isolated Phase 2 validation: native client/server and nightly browser atomics/build-std checks passed; all three gateway authentication/transport tests passed.
