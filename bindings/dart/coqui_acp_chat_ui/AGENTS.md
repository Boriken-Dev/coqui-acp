# coqui_acp_chat_ui: API header

Flutter, `import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';`. Depends on `coqui_acp` and `coqui_acp_chat` (paths `../coqui_acp`,
`../coqui_acp_chat`: their types are what the widgets draw), `markdown` and `flutter_markdown_plus`. Builds for the web (JavaScript and
WebAssembly) and desktop; nothing here reads a file, opens a socket or touches `dart:io`. Not published; alpha.

The widgets take **data and callbacks**, never a session class: an app that keeps its own session state, and one that uses
`ChatSession` from `coqui_acp_chat`, draw the same widgets. Everything the app owns (sources, worktrees, profiles, remembered rules,
modes, a browser panel) enters through a slot, never a fork.

## ChatLook

`ChatLook extends ThemeExtension<ChatLook>`, read as `context.chatLook` (the theme's, else made from its `ColorScheme`).

- Tokens: `bg`, `surface`, `raised`, `text`, `muted`, `accent`, `border`, `success`, `warning`, `danger`.
- Roles that follow the tokens unless a preset gives them: `user` (raised), `agent` (surface), `notice` (surface), `composer` (raised),
  `code`, `added` (success over bg), `removed` (danger over bg).
- Sizes: `messageWidth` 780, `gap` 22, `padding` 12, `radius` 12, `bodySize` 16, `codeFont` `'monospace'`.
- Switches: `agentInBubble` (false), `userToTheRight` (true), `monospace`, `userPrefix` (such as `'> '`).
- `density: ChatDensity` (`normal`, `thinking`, `verbose`; `.label`, `ChatDensity.named`): what a transcript folds, below.
- `ChatLook.fromScheme(scheme, {density, messageWidth, ...})`, `copyWith`, `lerp`. An app's look is a preset: build one, put it in
  `ThemeData(extensions: [...])`.

## ChatTranscript

`ChatTranscript({entries, terminals, plan, working, renderers, density, controller, narrow})`: the conversation as a list, one
selection over all of it (Ctrl+A, Ctrl+C and the menu give the whole text, built or not), the end followed as entries arrive until the
person scrolls away, a "Jump to latest" button. Its state (`ChatTranscriptState`, by a `GlobalKey`) has `jumpToLatest()`, `following`
and `selection` (`SelectableTranscriptState.copyAll()`).

- `entries` are `ChatEntryData` (`role`, `text`, `at`, `tool`, `compaction`): `ChatEntry` for plain ones, `ModelEntry(ChatMessage)` for
  `coqui_acp_chat`'s, or implement the interface. `terminals` is what commands printed, by terminal id. `plan` is drawn last as a
  checklist (`Plan: n of m`, a step done, the one running, those to come).
- `density` (null: the look's): **verbose** shows every entry as it is; **thinking** folds each run of finished tool calls into one
  line ("Edited calc.js, ran a command +1 −1") that opens into their cards, and keeps thoughts; **normal** also hides thoughts and
  folds a finished turn, from its first piece of work to its last, into "Worked for 1m 29s", which opens into what the agent did;
  what it said after its last piece of work stays. A tool call that failed, waits or runs is never folded, and a turn holding one does
  not fold. `working` keeps the last turn from folding. The duration is from the person's message to the last tool call or thought.
- `foldTranscript(entries, {density, working}) -> List<TranscriptItem>` (`EntryItem`, `BurstItem`, `WorkItem`) is the folding alone;
  `summarizeBurst(tools) -> ({text, added, removed})` and `spanLabel(duration)` are its words.

## Renderers

`ChatRenderers({entry, tool, block, burst, work, plan})`, given to `ChatTranscript(renderers:)` and read below it through
`ChatScope`. Every field defaults to the package's own drawing; `a.merge(b)` puts `b` first and over.

- `entry: List<Widget? Function(BuildContext, ChatEntryData)>`: tried in order for each entry; null passes. This is where a host's own
  content goes (a chart, a page, a delegation card, an A2UI surface).
- `tool: Map<String, Widget Function(BuildContext, ToolCall)>`: a card for a tool `kind`.
- `block: List<Widget? Function(BuildContext, ToolCall, ToolBlock)>`: one piece of a tool call's content.
- `burst`, `work`: the line a burst of finished tool calls, or a finished turn's work, folds into (`summary`, and the `cards` or `items`
  that open under it); `plan`: the plan.

## Cards and the queue

- `PermissionCard({agent, title, options, onChoose, detail, bypassPassed, forPerson, more, onCancel, notes, extraAnswers, host,
  rememberedBy})` and `PermissionChoice({option, onPressed})`: the agent's answers by its own names, refusals first, once before always.
  `notes` are lines under the question and `extraAnswers` buttons beside the agent's (an app's own "Always allow here"). `host` names the
  app in the card's sentences. Everything the agent wrote is cleaned of control and direction characters.
- `QuestionCard({request: AcpElicitation, agent, onAnswer, more, host})`: a form from the question's schema; a new `request` object
  starts a new form.
- `QueuePanel({lines: List<QueuedLine>, onEdit, onRemove, onClear, held, working, onSendNext, dialog})`: messages waiting for the turn
  to end, editable and removable; `dialog` shows the edit with the app's own dialog function.
- `BypassBanner({onLeave})`, `UsageFooter({text})`, `PlanPanel({entries})` (the plan folded to a line, for an app that keeps it above
  the composer).

## Secret prompts

The two cards of the secret-prompt extension (`docs/secrets.md` of coqui-acp, `package:coqui_acp/secret.dart`): a form for values some of which
the model must never see, and a display of values the person chose to look at. The app supplies the identity; the cards do the sealing.

- `SecretCardEnvironment({prover, verifier, expected, host, now})`: `prover` is this frontend's `AcpSecretProver` (its key stays wherever the app
  keeps it: the library takes functions), `verifier` an `AcpSecretVerifier` whose `own` is that identity and that knows the host, `expected` the
  host's `AcpIdentityId` (a request from any other identity is refused though the verifier knows it), `host` what the app calls itself, `now` a
  clock in Unix seconds for tests.
- `SecretRequestCard({request: AcpSecretRequestParams, environment, onReply, agent, more})`: checks the receiver's key binding and the digest of
  what is asked before it draws anything (a request that fails shows no fields and offers only Cancel); then one row per question in Settings'
  grammar (title, words, control): text, number, choice, choices, switch, each tagged "Sealed: only the host can read it" or "The agent sees this" (plain answers go to the agent, whatever the title says). A secret field (the schema's `writeOnly` or `x-sensitive`, never its
  `format` or name) is masked as typed, cannot be selected, has no context menu, no suggestions and no autofill. Send sealed seals each secret value to
  the host's per-request key, binds the whole answer, replaces the form with a final state (the fields and their undo history gone) and calls `onReply(AcpSecretAccepted)` once; Decline and Cancel call it with
  `AcpSecretDeclined` and `AcpSecretCancelled`. **`onReply` is the card's only output**: the app sends that to the host, and nothing else carries a
  value. A new `request` object starts a new form; typed text is wiped when the card leaves.
- `SecretShowCard({open: AcpSecretShowOpen, environment, deliver: ValueListenable<AcpSecretShowDeliver?>, onReady, onDone, agent, more, copy})`: asks
  first (Show, Decline, Cancel; nothing is opened before the person says so). Show makes the per-request key bound to this identity and calls
  `onReady(AcpSecretShowOpened.ready(...))`; when the host's second step reaches `deliver` the card refuses it unless its session and request id are
  this card's, opens it (the sender must be `expected`) and draws each value masked, with Show/Hide and Copy per secret field. Copy writes the clipboard
  only when pressed (`copy` replaces the system's for a test). `onDone(true)` on Done, `onDone(false)` on Cancel or a refusal; leaving the card
  overwrites the opened values and the private key.
- `SecretRequestEntry` and `SecretShowEntry` are `ChatEntryData` (role notice; `text` is the message, never a value), and
  `secretRenderers(environment)` is the `ChatRenderers` that draws them as the cards, merged like any other: `mine.merge(secretRenderers(env))`.

## Composer

`Composer({controller, onSend, focusNode, enabled, working, starting, onStop, commands, centred, hint, header, attachments, pickFiles,
pickFolder, clipboard, pasteAsFile, before, after, trailing, plusItems, onShiftTab, recall, actions, commandsTitle, actionsTitle,
mentions})`: the field, `/` suggestions from the agent's commands (and the host's own `actions`), `@` mentions from the host, paste, the attachment chips, the `+` menu, and Send (Queue and Stop while `working`, a spinner while `starting`).
`ComposerState.onKey` handles Shift+Tab (`onShiftTab`), Up in an empty message (`recall`: the last message sent, or null) and Ctrl+V (⌘V)
itself, whatever `focusNode` it is given; `paste()` is public.

- Slots: `header` above the message; `before` and `after` the `+` at the row's left; `trailing` beside Send; `plusItems` at the top of the
  `+` menu. A source picker, a mode or model chooser, an overlays control go there; `ComposerPill({label, icon, leading, danger, status, enabled})` is
  the control's look; `leading` takes the app's own mark (a source's or an agent's logo) where the icon would go, drawn at the pill's size and colour.
- `ComposerAttachments` (the app implements it): `canAttach`, `chips: List<ComposerChip>`, `addFiles(List<PickedFile>)`,
  `addPaths`, `addImage`, `addText`, `addFolder`, `remove(key)`; each `add` answers problems to tell the person. The package adds the
  "Add files or images…", "Add a folder reference…" and "Paste from clipboard" entries when it is given one, using `pickFiles` and
  `pickFolder`. `ClipboardReader` (`image`, `files`, `text`) is the paste source; `TextClipboard` is the default.
- `actions: List<ComposerAction>` (`name`, `description`, `run`): the host's own `/` entries. With any, `/` lists two groups under titles
  (`commandsTitle`, default "Agent commands", and `actionsTitle`, default "Host actions"); choosing an action runs it, clears the typed
  text and sends nothing. With none, the list is the agent's commands alone, untitled. `hint` is the placeholder, so a host changes it
  whenever its state does; it is kept to one line and ends in an ellipsis when the field is narrower.
- `mentions: ComposerMentions` (the app implements it): `search(query) -> Future<List<MentionCandidate>>` and `pick(candidate) -> String?`
  (a problem to tell the person, or null once taken). An `@` at the start or after whitespace, up to the caret, lists up to 8
  candidates (`MentionCandidate({label, detail, folder, key})`); choosing one writes `@label ` in its place after `pick` accepts it, and a
  refusal is shown and leaves the text. An answer to a query that has since changed is dropped. `mentionAt(text, caret)` finds the `@`.
  The package never reads a file: what a mention is, and what it may name, is the host's.
- `UsagePill({report, warnPermille, now})` and `UsageMenu`: an account's usage buckets (`UsageReport` of `coqui_acp_chat`); the words
  are in `usage_text.dart` (`pillText`, `bucketLine`, `headlineOf`, `levelOf`, `describeUsage`, `spanText`, ...).

## Changes and diffs

`changesOf(entries) -> List<FileChange>` (path, before, after, patches, `added`, `removed`, `lines`, `version`),
`ChangesPane({files, isViewed, onViewed, onComment, onClose, color, dialog})`: files as the agent sent them, a mark of viewed per file
version, and a comment on a line written into the message. `diffLines`, `withContext`, `patchLines`, `DiffView`.

## Also exported

`ToolCard`, `ToolEntry`, `ToolBlockView`, `CodeBox`, `TerminalBlock`, `TranscriptEntry`, `AgentMarkdown` (an image shows only its
description, a link is copied, never opened), `CopyButton`, `Thought`, `CompactionView`, `MessageBox`, `FoldLine`, `SummaryText`,
`ToolBurst`, `TurnWork`, `PlanChecklist`, `conversationText(entries)`, `plainMarkdown(text)`, `shortLine(value)`.

## Gotchas

- Tests draw with the test font (every glyph a square); the goldens in `test/goldens/` show layout and colour. Regenerate with
  `flutter test --update-goldens test/goldens_test.dart` and look at them.
- New widget tests alone with `--concurrency 1`.
- `example/` is a scripted conversation: `flutter build web --release` (JavaScript) and `--wasm`.
