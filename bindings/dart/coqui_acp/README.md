# coqui_acp

A client for the [Agent Client Protocol](https://agentclientprotocol.com), version 1 and version 2, over any stream of JSON-RPC
messages, for browsers and Flutter. It speaks whichever version the agent selects and never retries anything unacknowledged. It
authenticates nobody: give it a channel only after the peer is proven. Not published; the API may change in any release before 1.0.

## Quick start

```dart
final acp = AcpClient(incoming: lines, send: (line) async => socket.add(line));
await acp.initialize();
final description = await acp.describe();
if (description.options case AcpKnown(:final value)) {
  final models = value.where((option) => option['category'] == 'model');
  // show them to the person
}
final session = await acp.openSession(cwd: '/work');
await acp.prompt(session.id, 'Say hello');
```

## What an agent offers before a session

`describe()`, after `initialize()` and before any session, opens none. Each part is `AcpKnown(value)`, `AcpUnknown(reason)` when the
agent offers the question and could not tell, or `AcpNotSupported()` when it does not offer it, in which case nothing is sent.

- `options`: ACP's `SessionConfigOption`s as JSON maps, of the version spoken (`id` in v1, `configId` in v2), at the values a session
  starts with. A model is the option of category `model`, an effort the one of category `thought_level`. From the
  `_coqui_acp/session_options` request, sent only when the agent's capabilities advertise `dev.boriken.coqui.acp/session_options` in
  `_meta` (`acpSessionOptionsMethod`, `acpSessionOptionsCapability`).
- `modes`: ACP's v1 `SessionModeState`, from the same request. v2 has no modes, so it is not supported there.
- `sessions`: `session/list`, as `AcpSessionInfo`, when v1's `sessionCapabilities.list` is set; in v2 always asked, and a
  method-not-found answer is not supported. Pages are followed by their `nextCursor` until one gives none or an empty one, the
  sessions limit is reached, or the page limit is. The answer is an `AcpSessionList`: its `sessions`, and `incomplete`, null only
  when they are all the agent listed, otherwise `a later page failed: <why>`, `the sessions limit was reached` or `the page limit
  was reached`. A first page that fails decides the answer; a later one ends the list with the pages before it. A page limit of 0
  asks nothing, and the answer is `AcpUnknown`.

A failed or late answer is `AcpUnknown`. What the agent says is untrusted and only read: items that are not ACP's shape are skipped,
`_meta` is dropped, and `describeLimits` on the client bounds the rest. Each field of `AcpDescribeLimits` takes any value from 0 up:

| Field | Default | Bounds |
| --- | --- | --- |
| `options` | 64 | options kept |
| `values` | 1024 | values kept per option, across its groups |
| `modes` | 64 | modes kept |
| `sessions` | 500 | sessions kept, across pages |
| `text` | 4096 | UTF-8 bytes: a longer identifier drops its item, and other text is cut there |
| `pages` | 10 | `session/list` requests: the 500 sessions at 50 a page, and at most ten request timeouts on an agent that always gives a cursor |

```dart
final acp = AcpClient(
  incoming: lines,
  send: (line) async => socket.add(line),
  describeLimits: const AcpDescribeLimits(sessions: 50, pages: 2),
);
```

The Rust client keeps the same, with the same defaults, as `vectors/describe.json` says.

A person's choice is set once the session exists, with `setConfigOption` or `setMode`.

## Sessions

`openSession`, `prompt`, `cancel`, `closeSession`; `listSessions` and `reopenSession` for earlier sessions; `events` for one
normalized stream of `AcpEvent`s and `updates` for every `session/update` as it arrives. `takesImages` and `takesFiles` say what an
`AcpAttachment` may be. `package:coqui_acp/page.dart` is a person's agents from a page, over `document.acp`.

## Development

```sh
dart pub get
dart analyze
dart test -p vm
```

The conversations in `vectors/` at the repository's root are shared with the Rust client: a change in how this client behaves on the
wire changes them.

## License

[MPL-2.0](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE).
