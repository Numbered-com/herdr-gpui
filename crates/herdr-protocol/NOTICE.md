# Vendored Protocol Attribution

The wire declarations in `src/wire.rs`, handshake declarations in
`src/endpoint.rs`, and endpoint JSON fixtures in `tests/fixtures/` are adapted
from Herdr, https://github.com/herdrdev/herdr, licensed under Apache-2.0.
The license is reproduced in `LICENSE-APACHE` in this directory.

Source: Herdr 0.9.1, commit
[`856b64b9bfd41b9d3d82e2375ed5b4bf941fda28`](https://github.com/herdrdev/herdr/commit/856b64b9bfd41b9d3d82e2375ed5b4bf941fda28).
Sources: `src/protocol/{wire,endpoint}.rs`, `src/input/model.rs`,
`src/api/schema/common.rs`, `src/config/model.rs`, and
`tests/fixtures/endpoint-{hello,welcome,snapshot}-v1.json`.
No upstream NOTICE file was present.

Modifications: removed crossterm, ratatui, server/config conversion methods,
internal-only input types, native bincode Decode derives, and server builders;
inlined WindowsKeyRecord, AgentStatus, and ToastHerdrPosition; reformatted
declarations. All retained bincode field and variant orders are unchanged.
Framing was reimplemented with outbound bounds, decoder limits and strict
payload consumption. Surface validation and patch application are client code.

The client discovery rules also adapt `src/server/socket_paths.rs`,
`src/session.rs`, and `src/config/io.rs` from the same source/license.
The native client's build mode does not implicitly select Herdr's dev session.

Client-local activity presentation adapts the seen/completion rules from
`src/client/shell/endpoint_agent_state.rs` and status priority from
`src/client/shell.rs`, under the same license. The GUI adds asynchronous
presentation fences so coalesced socket updates do not acknowledge unseen output.
