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

## Composer

`Composer({controller, onSend, focusNode, enabled, working, starting, onStop, commands, centred, hint, header, attachments, pickFiles,
pickFolder, clipboard, pasteAsFile, before, after, trailing, plusItems, onShiftTab, recall})`: the field, `/` suggestions from the agent's
commands, paste, the attachment chips, the `+` menu, and Send (Queue and Stop while `working`, a spinner while `starting`).
`ComposerState.onKey` handles Shift+Tab (`onShiftTab`), Up in an empty message (`recall`: the last message sent, or null) and Ctrl+V (⌘V)
itself, whatever `focusNode` it is given; `paste()` is public.

- Slots: `header` above the message; `before` and `after` the `+` at the row's left; `trailing` beside Send; `plusItems` at the top of the
  `+` menu. A source picker, a mode or model chooser, an overlays control go there; `ComposerPill({label, icon, danger, status, enabled})` is
  the control's look. A source mark the app draws (its own icon widget) is part of the app's widget in a slot.
- `ComposerAttachments` (the app implements it): `canAttach`, `chips: List<ComposerChip>`, `addFiles(List<PickedFile>)`,
  `addPaths`, `addImage`, `addText`, `addFolder`, `remove(key)`; each `add` answers problems to tell the person. The package adds the
  "Add files or images…", "Add a folder reference…" and "Paste from clipboard" entries when it is given one, using `pickFiles` and
  `pickFolder`. `ClipboardReader` (`image`, `files`, `text`) is the paste source; `TextClipboard` is the default.
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
