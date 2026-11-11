# coqui_acp_chat: API header

Pure Dart, `import 'package:coqui_acp_chat/coqui_acp_chat.dart';`. Depends only on `coqui_acp` (path `../coqui_acp`; its protocol
types are this package's input). No Flutter, no `dart:io`, no `dart:html`: it runs on the VM and in browsers, and a test checks the
import graph. Not published; alpha.

## The shape

`ChatStore` holds `ChatSession`s. A session is one conversation: its state is plain fields and lists, it tells `addListener`
listeners after each change and `events` (a stream) for what happens once. A `ChatConnection` carries the agent's updates in and the
session's requests out; the session never opens, verifies or retries anything. Everything an agent sends is bounded by `ChatLimits`
and read as untrusted: wrong types are ignored, text is cut, names lose control and bidi characters.

## Connection

- `abstract class ChatConnection`: `updates: Stream<AcpSessionUpdate>`, `events: Stream<AcpEvent>` (a v1 turn's end, lost output, a
  question answered cancelled), `isClosed`, `protocolVersion`, `prompt(sessionId, text, {attachments}) -> Future<String?>` (v2: the
  inserted message's id; v1: completes when the turn ends), `cancel`, `setMode`, `setConfigOption -> Future<Map>`, `close`. Optional:
  `reportsUsage` (false), `usageNotices`, `fetchUsage`.
- `AcpChatConnection(AcpClient)`, or `AcpChatConnection.routed(requests, (decider, elicitor) => AcpClient(...))`, which wires the
  client's decider and elicitor to `requests` (a `ChatRequests`, which `ChatStore` is) under this connection.
- Permission and form requests are routed by the connection they came on **and** the session id: an agent picks its own ids, so two
  agents can both say `s1`. `ChatStore.askPermission(request, from)` finds the session whose `connection` is `from`; none: `AcpDeny`
  (question: `AcpElicitCancel`).

## ChatStore (extends `ChatNotifier`)

`ChatStore({limits, recorder, hostNoticesKey, clock})`. `add({title, agent, peerId, sessionId, cwd}) -> ChatSession` (inserted first,
selected), `sessions`, `selected`, `selectedId`, `select(id)` (clears `unread`), `waiting` (sessions with something for a person),
`attention: Stream<Attention>` (`({session, kind})`, `AttentionKind` permission / question / finished; a stopped turn is not news, a
finished turn in a session not selected sets `unread`), `forget(session)` (detach, keep the connection), `close(session)` (answer what
waits cancelled, end the connection when no other session uses it), `dispose()`.

## ChatSession (extends `ChatNotifier`)

`ChatSession({required id, title, agent, peerId, sessionId, cwd, limits, recorder, hostNoticesKey, clock, agentNames = true})`.

- **State**: `messages: List<ChatMessage>` (`role` user/agent/notice/thought/tool/compaction, `text`, `at`, `messageId`, `tool`,
  `compaction`, `sentBy`, `turn`), `toolEntries`, `compactions`, `terminals`, `plan: List<PlanEntry>`, `commands`, `options:
  List<ConfigOption>`, `usage: SessionUsage`, `usageReport`, `spend`, `reportsUsage`, `status` (`disconnected`/`connected`/
  `working`), `title`, `unread`, `turns: List<ChatTurn>` (`startedAt`, `endedAt`, `outcome`, `duration(now)`), `currentTurn`.
- **Input**: `apply(AcpSessionUpdate)` (a connection's updates arrive here by themselves), `applyOpened(AcpOpenedSession)`,
  `attach(connection)` / `detach()` (a replaced or ended connection **withdraws** what it asked), `reopen(open)` (a replay is history,
  not news; tool lines left open become `stopped`), `rename(name)` (the agent stops naming it).
- **Prompts**: `send(text, {attachments, from}) -> Future<void>`: sent now, or queued while a turn runs (or a message is being
  handed over); throws `StateError` past `queued`. `steer(text, ...)` puts it first and cancels the turn, which then goes on with it
  (a turn ended to steer does not hold the queue). `steerQueued(message)`, `sendQueued([message])`, `editQueued`, `removeQueued([m])`,
  `queue`, `queueHeld` (why the queue waits: `the turn was stopped`, `the turn failed`, `it could not be sent: ...`; null = goes on),
  `cancel({from})` (answers open requests cancelled, then asks the agent).
- **Requests**: `pendingPermissions: List<PermissionQuestion>` (`title`, `kind`, `detail`, `options` in the agent's order, `forPerson`,
  `toolCallId`, `outcome`), `pendingQuestions: List<AgentQuestion>`. Answer with `answerPermission(allow:, by:)`,
  `choosePermission(option, by:)`, `cancelPermission`, `settle(question, decision, by:)`, `answerQuestion(answer, by:)`,
  `settleQuestion`; each returns whether **it** settled the request (the first answer wins). `settleElsewhere(question, by:,
  optionId:)` records an answer learned from elsewhere. `settledPermissions`/`settledQuestions` keep the last `settledRequests`
  with `outcome` (`Settled`: `how` answered/cancelled/withdrawn, `by`, `at`, `answer`).
- **Options**: `choose(option, value) -> Future<bool refused>` (waits in `pending` while a turn runs and goes before the next prompt),
  `mode`, `underlyingMode`, `inBypass`, `setBypass(on)` (answers requests that offer an allow, never a question or a plan).
- **Clients**: `attachClient(id, {name})`, `detachClient(id)`, `clients`; `from:`/`by:` must name an attached client (or
  `bypassClient`), else `StateError`. `ClientEvent` goes out on `events`.
- **Replay**: `replay() -> Iterable<AcpSessionUpdate>` rebuilds messages (with `sentBy` in `_meta[chatClientMetaKey]`), tool calls,
  terminals, compactions, plan, commands, options, usage, title and running state in another `ChatSession`. Notices the chat wrote,
  waiting requests (read `pendingPermissions`) and `stopped` (goes as `cancelled`) are not carried. `chatClientMetaKey` is provisional.
- **Events** (`events`, `sealed ChatEvent`): `AttentionEvent(kind)`, `PermissionSettledEvent`, `QuestionSettledEvent`, `ClientEvent`.

## Content types

`ToolCall` (`id`, `title`, `kind`, `status: ToolStatus` pending/running/done/failed/cancelled/stopped, `blocks`, `locations`,
`isOpen`, `changedPaths`) with `ToolBlock`: `ToolText`, `ToolDiff(path, oldText?, newText)`, `ToolPatch(text, paths)`, `ToolTerminal(terminalId)`.
`TerminalView(command, output, exitCode)`; `PlanEntry`/`PlanStatus`; `Compaction`/`CompactionStatus`; `AgentCommandInfo`;
`ConfigOption`/`Choice`/`OptionRole`/`roleOf`/`optionIn`/`modeOptionIn`/`PendingChoice`; `isBypassMode`, `bypassAnswer`,
`ownBypass`; `SessionUsage`; `UsageReport` (the `dev.boriken.coqui.acp/usage` extension: `parse`, `toJson`, buckets, spend) with the
words a usage control shows (`pillText`, `figureOf`, `compactCount`, `dollars`, ...); `reasonOf(error)`, `titleFrom(text)`, `toolRuns(entries)` (consecutive tool calls grouped, for a transcript that folds them).

## Recording

`UpdateRecorder({write})` records each update's kind and shape (never its text) and `unshown`, the kinds that reached no rendering;
`renderingOf(update, limits) -> Rendering`. A host's test fails when `unshown` is not empty.

## ChatLimits (every field: any value from 0 up)

`entries` 5000, `messageText` 2 MiB, `pendingPermissions` 64, `pendingQuestions` 16, `settledRequests` 32, `queued` 20, `clients` 16,
`turns` 1000, `toolText` 256 KiB, `terminalOutput` 256 KiB (kept from its end), `planEntries` 200, `commands` 500, `configOptions` 32,
`choices` 200, `toolBlocks` 64, `locations` 64, `shortText` 512, `terminals` 256, `describedInput` 4000, `hostNotices` 4,
`listShapes` 8, `shapeDepth` 8, `titleLength` 60. A request past `pendingPermissions`/`pendingQuestions` is answered cancelled and a notice says so.

## Gotchas

- A tool line from a connection that ended is `stopped` and its request `withdrawn`: **nothing is sent to the agent**.
- `status` is `working` from a v2 `state_update` running, or at once for a v1 prompt; a v1 turn ends by the connection's `AcpCompleted`
  /`AcpCancelled`/`AcpFailed` event.
- `hostNoticesKey` (default none) names the `_meta` key of the host's notices in the answer to `session/new`; they show as the
  agent's word, never the chat's.
- Attachments are `AcpAttachment`s already built; reading files, size checks and what an agent takes are the host's.
- Tests: `dart test -p vm`; `dart test -p chrome` for the browser (the files tagged `@TestOn('vm')` read files and are skipped there).
