import 'dart:async';

import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:flutter/foundation.dart' show defaultTargetPlatform, TargetPlatform;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'look.dart';
import 'usage_text.dart';

/// What was copied: an image, files by their paths, or text.
abstract interface class ClipboardReader {
  Future<Uint8List?> image();
  Future<List<String>> files();
  Future<String?> text();
}

/// Text only, which is all Flutter's own clipboard reads.
final class TextClipboard implements ClipboardReader {
  const TextClipboard();
  @override
  Future<Uint8List?> image() async => null;
  @override
  Future<List<String>> files() async => const [];
  @override
  Future<String?> text() async => (await Clipboard.getData(Clipboard.kTextPlain))?.text;
}

/// A file the person picked, as the composer takes it from the host's picker.
typedef PickedFile = ({String name, String path, String? mimeType, Future<Uint8List> Function() read});

/// What goes with the next message, as a chip shows it.
final class ComposerChip {
  const ComposerChip({
    required this.key,
    required this.name,
    required this.detail,
    this.kind = ChipKind.file,
    this.thumbnail,
    this.note,
    this.refused = false,
  });

  /// Names the chip to [ComposerAttachments.remove].
  final Object key;
  final String name;

  /// Its size, or what it is.
  final String detail;
  final ChipKind kind;

  /// An image's bytes, for a thumbnail.
  final List<int>? thumbnail;

  /// How it will reach the agent when that is not as it is, or why it cannot.
  final String? note;
  final bool refused;
}

enum ChipKind { image, text, file, folder }

/// What `@` can name: a file or folder the host knows, as the list shows it.
final class MentionCandidate {
  const MentionCandidate({required this.label, this.detail, this.folder = false, this.key});

  /// What is written after the `@` when it is chosen, and what the list shows.
  final String label;

  /// A second line, such as the folder it is in.
  final String? detail;
  final bool folder;

  /// What the host knows it by; [label] when null.
  final Object? key;
}

/// What the composer asks of a host that lets the person name things with `@`.
abstract interface class ComposerMentions {
  /// What [query], the text typed after the `@`, names, best first. Nothing the person typed is a path to read: it only
  /// narrows what the host chooses to offer.
  Future<List<MentionCandidate>> search(String query);

  /// Takes the choice of [candidate]. Answers why it cannot be taken, or null once it is; the text is then left as it was.
  String? pick(MentionCandidate candidate);
}

/// An action of the host's own that `/` lists beside the agent's commands. Choosing it runs [run] at once; nothing is sent.
final class ComposerAction {
  const ComposerAction({required this.name, required this.description, required this.run});
  final String name, description;
  final VoidCallback run;
}

/// The `@` being typed at [caret] in [text]: where it starts and the query after it, or null when the caret is not in one.
/// An `@` counts at the start or after whitespace, and the query ends at the first whitespace.
({int start, String query})? mentionAt(String text, int caret) {
  if (caret < 0 || caret > text.length) return null;
  final space = RegExp(r'\s');
  for (var i = caret - 1; i >= 0; i--) {
    if (text.codeUnitAt(i) == 0x40) {
      if (i > 0 && !space.hasMatch(text[i - 1])) return null;
      return (start: i, query: text.substring(i + 1, caret));
    }
    if (space.hasMatch(text[i])) return null;
  }
  return null;
}

/// What the composer asks of a host that takes attachments. Each `add` answers a problem to tell the person, or nothing.
abstract interface class ComposerAttachments {
  /// Whether anything can be added now.
  bool get canAttach;

  List<ComposerChip> get chips;

  /// Adds what the person picked, by path where the host can read paths and by bytes otherwise.
  Future<List<String>> addFiles(List<PickedFile> files);

  /// Adds files by their paths, such as copied files or dropped ones.
  Future<List<String>> addPaths(List<String> paths);

  /// Adds an image pasted from the clipboard.
  String? addImage(Uint8List bytes);

  /// Adds a long text as an attachment of its own.
  String? addText(String text);

  /// Adds a folder, to be linked by its path.
  String? addFolder(String path);

  void remove(Object key);
}

/// A compact control in the row under the message.
class ComposerPill extends StatelessWidget {
  const ComposerPill({super.key, required this.label, this.icon, this.leading, this.danger = false, this.status = false, this.enabled = true, this.maxWidth = 180});

  final String label;
  final IconData? icon;

  /// A widget drawn where [icon] would be, in the same size and colour: an app's own mark for a source or an agent.
  final Widget? leading;

  /// [danger] is for what is wrong; [status] for what is on, never the colour of an error.
  final bool danger, status, enabled;

  /// The widest the label gets before it is cut short.
  final double maxWidth;

  @override
  Widget build(BuildContext context) {
    final look = context.chatLook;
    final color = danger ? look.danger : status ? look.warning : (enabled ? null : look.muted);
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
      child: Row(mainAxisSize: MainAxisSize.min, children: [
        if (icon != null) ...[Icon(icon, size: 16, color: color ?? look.muted), const SizedBox(width: 4)],
        if (leading != null) ...[
          IconTheme(data: IconThemeData(size: 16, color: color ?? look.muted), child: leading!),
          const SizedBox(width: 4),
        ],
        Flexible(
          child: ConstrainedBox(
            constraints: BoxConstraints(maxWidth: maxWidth),
            child: Text(label, maxLines: 1, overflow: TextOverflow.ellipsis, style: TextStyle(fontSize: 12.5, color: color)),
          ),
        ),
        if (enabled) Icon(Icons.expand_more, size: 16, color: color ?? look.muted),
      ]),
    );
  }
}

/// The message being written, what goes with it, and a row of controls under it: `+`, the host's controls, and Send.
///
/// What is the host's goes in slots: [header] above the message, [before] and [after] the `+` at the left of the row, [trailing]
/// beside Send, [plusItems] in the `+` menu. The package supplies the field, the `/` suggestions, paste, the attachment chips and
/// the files and folder entries of `+`, and Send, Queue and Stop.
class Composer extends StatefulWidget {
  const Composer({
    super.key,
    required this.controller,
    required this.onSend,
    this.focusNode,
    this.enabled = true,
    this.working = false,
    this.starting = false,
    this.onStop,
    this.commands = const [],
    this.actions = const [],
    this.commandsTitle = 'Agent commands',
    this.actionsTitle = 'Host actions',
    this.mentions,
    this.centred = false,
    this.hint = 'Message agent',
    this.header,
    this.attachments,
    this.pickFiles,
    this.pickFolder,
    this.clipboard = const TextClipboard(),
    this.pasteAsFile = defaultPasteAsFile,
    this.before = const [],
    this.after = const [],
    this.trailing = const [],
    this.plusItems = const [],
    this.onShiftTab,
    this.recall,
  });

  /// The most characters a paste puts in the text; a longer one goes as an attachment of its own.
  static const defaultPasteAsFile = 8000;

  /// From this width the controls stay on one line.
  static const double wideFrom = 600;

  final TextEditingController controller;
  final FocusNode? focusNode;
  final VoidCallback onSend;

  /// Whether the field takes text and Send works.
  final bool enabled;

  /// Whether a turn is running: Send becomes Queue, and Stop appears.
  final bool working;

  /// Whether the conversation is starting: a spinner stands in for Send.
  final bool starting;
  final VoidCallback? onStop;

  /// The agent's `/` commands.
  final List<AgentCommandInfo> commands;

  /// The host's own `/` actions. With any, the list has two groups, titled [commandsTitle] and [actionsTitle]; with none it is
  /// the agent's commands alone, untitled.
  final List<ComposerAction> actions;
  final String commandsTitle, actionsTitle;

  /// What an `@` in the message names; null leaves `@` as text.
  final ComposerMentions? mentions;

  /// Whether it is the middle of a new session's empty view rather than docked under a conversation.
  final bool centred;
  final String hint;
  final Widget? header;
  final ComposerAttachments? attachments;
  final Future<List<PickedFile>> Function()? pickFiles;
  final Future<String?> Function()? pickFolder;
  final ClipboardReader clipboard;
  final int pasteAsFile;
  final List<Widget> before, after, trailing, plusItems;

  /// What Shift+Tab does in the field, usually putting the conversation in its next mode; null leaves the key alone.
  final VoidCallback? onShiftTab;

  /// The last message the person sent here, or null: Up in an empty message brings it back, and anywhere else moves the cursor as
  /// usual.
  final String? Function()? recall;

  @override
  State<Composer> createState() => ComposerState();
}

class ComposerState extends State<Composer> {
  void _say(String text) => ScaffoldMessenger.maybeOf(context)?.showSnackBar(SnackBar(content: Text(text)));

  void _sayAll(Iterable<String> problems) {
    if (problems.isNotEmpty) _say(problems.join('\n'));
  }

  Future<void> _addFiles() async {
    final host = widget.attachments;
    final picked = await widget.pickFiles?.call() ?? const <PickedFile>[];
    if (!mounted || host == null) return;
    final problems = await host.addFiles(picked);
    if (mounted) _sayAll(problems);
  }

  Future<void> _addFolder() async {
    final folder = await widget.pickFolder?.call();
    if (folder == null || !mounted) return;
    final problem = widget.attachments?.addFolder(folder);
    if (problem != null) _say(problem);
  }

  /// Chooses [candidate] for the `@` that starts at [start] and runs to [end]: the host takes it, and the text becomes `@label `.
  void _pickMention(MentionCandidate candidate, int start, int end) {
    final problem = widget.mentions?.pick(candidate);
    if (problem != null) {
      _say(problem);
      return;
    }
    final text = widget.controller.text;
    if (start < 0 || end > text.length || start > end) return;
    final inserted = '@${candidate.label} ';
    widget.controller.value = TextEditingValue(
      text: text.replaceRange(start, end, inserted),
      selection: TextSelection.collapsed(offset: start + inserted.length),
    );
  }

  /// What the clipboard holds: an image becomes an image attachment, copied files file attachments, and text stays text unless it
  /// is longer than [Composer.pasteAsFile] characters.
  Future<void> paste() async {
    final clipboard = widget.clipboard;
    final host = widget.attachments;
    final image = await clipboard.image();
    if (!mounted) return;
    if (image != null && image.isNotEmpty && host != null) {
      final problem = host.addImage(image);
      if (problem != null) _say(problem);
      return;
    }
    final files = await clipboard.files();
    if (!mounted) return;
    if (files.isNotEmpty && host != null) {
      _sayAll(await host.addPaths(files));
      return;
    }
    final text = await clipboard.text();
    if (text == null || text.isEmpty || !mounted) return;
    if (text.length > widget.pasteAsFile && host != null) {
      final problem = host.addText(text);
      if (problem != null) _say(problem);
      return;
    }
    final value = widget.controller.value;
    final selection = value.selection.isValid ? value.selection : TextSelection.collapsed(offset: value.text.length);
    final joined = value.text.replaceRange(selection.start, selection.end, text);
    widget.controller.value = TextEditingValue(
      text: joined,
      selection: TextSelection.collapsed(offset: selection.start + text.length),
    );
  }

  /// Shift+Tab goes to [Composer.onShiftTab], Up in an empty message brings back [Composer.recall], and Ctrl+V (⌘V on a Mac)
  /// pastes through [paste]; every other key goes on as usual.
  KeyEventResult onKey(FocusNode node, KeyEvent event) {
    if (event is KeyDownEvent &&
        event.logicalKey == LogicalKeyboardKey.tab &&
        HardwareKeyboard.instance.isShiftPressed &&
        !HardwareKeyboard.instance.isControlPressed &&
        !HardwareKeyboard.instance.isAltPressed &&
        widget.onShiftTab != null) {
      widget.onShiftTab!();
      return KeyEventResult.handled;
    }
    if ((event is KeyDownEvent || event is KeyRepeatEvent) && event.logicalKey == LogicalKeyboardKey.arrowUp && widget.controller.text.isEmpty) {
      final last = widget.recall?.call();
      if (last != null && last.isNotEmpty) {
        widget.controller.value = TextEditingValue(text: last, selection: TextSelection.collapsed(offset: last.length));
        return KeyEventResult.handled;
      }
    }
    if (event is! KeyDownEvent || event.logicalKey != LogicalKeyboardKey.keyV || !widget.enabled) return KeyEventResult.ignored;
    final keys = HardwareKeyboard.instance;
    final mac = defaultTargetPlatform == TargetPlatform.macOS || defaultTargetPlatform == TargetPlatform.iOS;
    if ((mac ? keys.isMetaPressed : keys.isControlPressed) && !keys.isAltPressed && !keys.isShiftPressed) {
      unawaited(paste());
      return KeyEventResult.handled;
    }
    return KeyEventResult.ignored;
  }

  @override
  Widget build(BuildContext context) {
    final look = context.chatLook;
    final chips = widget.attachments?.chips ?? const <ComposerChip>[];
    final field = TextField(
      controller: widget.controller,
      focusNode: widget.focusNode,
      enabled: widget.enabled,
      minLines: widget.centred ? 3 : 1,
      maxLines: widget.centred ? 10 : 5,
      textInputAction: TextInputAction.send,
      onSubmitted: (_) => widget.onSend(),
      decoration: InputDecoration(
        hintText: widget.hint,
        hintMaxLines: 1,
        border: InputBorder.none,
        isDense: true,
        contentPadding: const EdgeInsets.fromLTRB(4, 8, 4, 8),
      ),
    );
    final box = Container(
      key: const Key('composer'),
      padding: const EdgeInsets.fromLTRB(10, 6, 6, 6),
      decoration: BoxDecoration(color: look.composer, borderRadius: BorderRadius.circular(look.radius + 2)),
      child: Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.stretch, children: [
        ?widget.header,
        if (chips.isNotEmpty) AttachmentChips(chips: chips, onRemove: widget.attachments!.remove),
        Focus(canRequestFocus: false, skipTraversal: true, onKeyEvent: onKey, child: field),
        _Controls(composer: this),
      ]),
    );
    return Column(mainAxisSize: MainAxisSize.min, children: [
      ValueListenableBuilder<TextEditingValue>(
        valueListenable: widget.controller,
        builder: (context, value, _) {
          final caret = value.selection.isValid ? value.selection.baseOffset : value.text.length;
          final mention = widget.mentions == null ? null : mentionAt(value.text, caret);
          return Column(mainAxisSize: MainAxisSize.min, children: [
            CommandSuggestions(
              commands: widget.commands,
              actions: widget.actions,
              commandsTitle: widget.commandsTitle,
              actionsTitle: widget.actionsTitle,
              typed: value.text,
              onPick: (command) {
                final text = '/${command.name} ';
                widget.controller.value = TextEditingValue(text: text, selection: TextSelection.collapsed(offset: text.length));
              },
              onRun: (action) {
                widget.controller.value = const TextEditingValue();
                action.run();
              },
            ),
            if (mention != null)
              MentionSuggestions(
                key: const Key('mention-suggestions'),
                source: widget.mentions!,
                query: mention.query,
                onPick: (candidate) => _pickMention(candidate, mention.start, caret),
              ),
          ]);
        },
      ),
      const SizedBox(height: 4),
      box,
    ]);
  }
}

/// What goes with the next message, each with what it is, its size, how it will reach the agent when that is not as it is,
/// and a way to take it back.
class AttachmentChips extends StatelessWidget {
  const AttachmentChips({super.key, required this.chips, required this.onRemove});

  final List<ComposerChip> chips;
  final ValueChanged<Object> onRemove;

  @override
  Widget build(BuildContext context) {
    final look = context.chatLook;
    final error = look.danger;
    return Padding(
      padding: const EdgeInsets.only(bottom: 4, top: 2),
      child: Wrap(spacing: 6, runSpacing: 6, children: [
        for (final chip in chips)
          Builder(builder: (context) {
            final thumbnail = chip.kind == ChipKind.image && chip.thumbnail != null
                ? ClipRRect(
                    borderRadius: BorderRadius.circular(4),
                    child: Image.memory(Uint8List.fromList(chip.thumbnail!), width: 28, height: 28, fit: BoxFit.cover,
                        gaplessPlayback: true, errorBuilder: (_, _, _) => const Icon(Icons.image_outlined, size: 20)),
                  )
                : Icon(switch (chip.kind) {
                    ChipKind.folder => Icons.folder_outlined,
                    ChipKind.text => Icons.notes,
                    _ => Icons.description_outlined,
                  }, size: 20, color: look.muted);
            final view = Container(
              key: ValueKey('attachment ${chip.name}'),
              constraints: const BoxConstraints(maxWidth: 260),
              padding: const EdgeInsets.fromLTRB(4, 3, 2, 3),
              decoration: BoxDecoration(
                border: Border.all(color: chip.refused ? error : look.muted.withValues(alpha: 0.35)),
                borderRadius: BorderRadius.circular(8),
              ),
              child: Row(mainAxisSize: MainAxisSize.min, children: [
                thumbnail,
                const SizedBox(width: 6),
                Flexible(child: Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
                  Text(chip.name, maxLines: 1, overflow: TextOverflow.ellipsis, style: const TextStyle(fontSize: 12)),
                  Text(
                    chip.note ?? chip.detail,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(fontSize: 11, color: chip.refused ? error : look.muted),
                  ),
                ])),
                IconButton(
                  tooltip: 'Remove ${chip.name}',
                  visualDensity: VisualDensity.compact,
                  iconSize: 16,
                  onPressed: () => onRemove(chip.key),
                  icon: const Icon(Icons.close),
                ),
              ]),
            );
            return chip.note == null ? view : Tooltip(message: chip.note!, child: view);
          }),
      ]),
    );
  }
}

/// The row under the message.
class _Controls extends StatelessWidget {
  const _Controls({required this.composer});

  final ComposerState composer;

  @override
  Widget build(BuildContext context) {
    final widget = composer.widget;
    final host = widget.attachments;
    final left = <Widget>[
      ...widget.before,
      MenuAnchor(
        menuChildren: [
          ...widget.plusItems,
          if (host != null) ...[
            MenuItemButton(
              key: const Key('add-files'),
              leadingIcon: const Icon(Icons.attach_file, size: 18),
              onPressed: widget.pickFiles == null ? null : () => unawaited(composer._addFiles()),
              child: const Text('Add files or images…'),
            ),
            MenuItemButton(
              key: const Key('add-folder'),
              leadingIcon: const Icon(Icons.folder_outlined, size: 18),
              onPressed: widget.pickFolder == null ? null : () => unawaited(composer._addFolder()),
              child: const Text('Add a folder reference…'),
            ),
            MenuItemButton(
              key: const Key('paste'),
              leadingIcon: const Icon(Icons.content_paste, size: 18),
              onPressed: () => unawaited(composer.paste()),
              child: const Text('Paste from clipboard'),
            ),
          ],
        ],
        builder: (context, controller, _) => IconButton(
          key: const Key('composer-plus'),
          tooltip: 'Add to the message',
          visualDensity: VisualDensity.compact,
          onPressed: widget.enabled && (host?.canAttach ?? widget.plusItems.isNotEmpty)
              ? () => controller.isOpen ? controller.close() : controller.open()
              : null,
          icon: const Icon(Icons.add),
        ),
      ),
      ...widget.after,
    ];
    final right = widget.trailing;
    final send = widget.working
        ? Row(mainAxisSize: MainAxisSize.min, children: [
            IconButton(tooltip: 'Queue', onPressed: widget.enabled ? widget.onSend : null, icon: const Icon(Icons.schedule_send_outlined)),
            IconButton(tooltip: 'Stop', onPressed: widget.onStop, icon: const Icon(Icons.stop_circle_outlined)),
          ])
        : widget.starting
        ? const Padding(padding: EdgeInsets.all(12), child: SizedBox.square(dimension: 18, child: CircularProgressIndicator(strokeWidth: 2)))
        : IconButton.filled(
            tooltip: 'Send',
            onPressed: widget.enabled ? widget.onSend : null,
            icon: const Icon(Icons.arrow_upward_rounded),
          );
    return LayoutBuilder(builder: (context, box) {
      // On a narrow window the controls go on to a second line, and Send stays at the end.
      if (box.maxWidth < Composer.wideFrom) {
        return Row(crossAxisAlignment: CrossAxisAlignment.end, children: [
          Expanded(child: Wrap(crossAxisAlignment: WrapCrossAlignment.center, spacing: 2, runSpacing: 2, children: [...left, ...right])),
          send,
        ]);
      }
      // Wide, it stays on one line: the row is as wide as the composer, and scales down rather than wrap or overflow.
      return FittedBox(
        fit: BoxFit.scaleDown,
        alignment: Alignment.centerLeft,
        child: ConstrainedBox(
          constraints: BoxConstraints(minWidth: box.maxWidth),
          child: Row(mainAxisAlignment: MainAxisAlignment.spaceBetween, children: [
            Row(mainAxisSize: MainAxisSize.min, children: left),
            Row(mainAxisSize: MainAxisSize.min, children: [...right, send]),
          ]),
        ),
      );
    });
  }
}

/// The agent's commands, and the host's own actions, matching what is being typed. Nothing is sent until the person sends it; an
/// action runs when chosen.
class CommandSuggestions extends StatelessWidget {
  const CommandSuggestions({
    super.key,
    required this.commands,
    required this.typed,
    required this.onPick,
    this.actions = const [],
    this.onRun,
    this.commandsTitle = 'Agent commands',
    this.actionsTitle = 'Host actions',
  });

  /// The most each group lists.
  static const int shown = 6;

  final List<AgentCommandInfo> commands;
  final List<ComposerAction> actions;
  final String commandsTitle, actionsTitle;
  final String typed;
  final void Function(AgentCommandInfo command) onPick;
  final void Function(ComposerAction action)? onRun;

  @override
  Widget build(BuildContext context) {
    if (!typed.startsWith('/') || typed.contains(' ')) return const SizedBox.shrink();
    final wanted = typed.substring(1).toLowerCase();
    final found = commands.where((command) => command.name.toLowerCase().startsWith(wanted)).take(shown).toList();
    final foundActions = actions.where((action) => action.name.toLowerCase().startsWith(wanted)).take(shown).toList();
    if (found.isEmpty && foundActions.isEmpty) return const SizedBox.shrink();
    final look = context.chatLook;
    final titled = actions.isNotEmpty;
    Widget title(String text, Key key) => Padding(
      key: key,
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
      child: Align(
        alignment: Alignment.centerLeft,
        child: Text(text, style: TextStyle(fontSize: 11.5, fontWeight: FontWeight.w600, color: look.muted)),
      ),
    );
    return Material(
      color: look.composer,
      borderRadius: BorderRadius.circular(look.radius / 2),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (titled && found.isNotEmpty) title(commandsTitle, const Key('commands-group')),
          for (final command in found)
            ListTile(
              dense: true,
              visualDensity: VisualDensity.compact,
              title: Text('/${command.name}', style: TextStyle(fontFamily: look.codeFont)),
              subtitle: command.description.isEmpty ? null : Text(command.description, maxLines: 1, overflow: TextOverflow.ellipsis),
              onTap: () => onPick(command),
            ),
          if (foundActions.isNotEmpty) title(actionsTitle, const Key('actions-group')),
          for (final action in foundActions)
            ListTile(
              key: ValueKey('action ${action.name}'),
              dense: true,
              visualDensity: VisualDensity.compact,
              title: Text('/${action.name}', style: TextStyle(fontFamily: look.codeFont)),
              subtitle: action.description.isEmpty ? null : Text(action.description, maxLines: 1, overflow: TextOverflow.ellipsis),
              onTap: () => onRun?.call(action),
            ),
        ],
      ),
    );
  }
}

/// What the host's [ComposerMentions] offers for the `@` being typed: the files and folders matching [query], each with where it
/// is. Choosing one goes to [onPick]. A search that is still out when the query changes is ignored.
class MentionSuggestions extends StatefulWidget {
  const MentionSuggestions({super.key, required this.source, required this.query, required this.onPick});

  /// The most the list shows.
  static const int shown = 8;

  final ComposerMentions source;
  final String query;
  final void Function(MentionCandidate candidate) onPick;

  @override
  State<MentionSuggestions> createState() => _MentionSuggestionsState();
}

class _MentionSuggestionsState extends State<MentionSuggestions> {
  List<MentionCandidate> _found = const [];
  var _asked = 0;

  @override
  void initState() {
    super.initState();
    _search();
  }

  @override
  void didUpdateWidget(MentionSuggestions old) {
    super.didUpdateWidget(old);
    if (old.query != widget.query || old.source != widget.source) _search();
  }

  Future<void> _search() async {
    final mine = ++_asked;
    List<MentionCandidate> found;
    try {
      found = await widget.source.search(widget.query);
    } on Object {
      found = const [];
    }
    if (!mounted || mine != _asked) return;
    setState(() => _found = found.take(MentionSuggestions.shown).toList());
  }

  @override
  Widget build(BuildContext context) {
    if (_found.isEmpty) return const SizedBox.shrink();
    final look = context.chatLook;
    return Material(
      color: look.composer,
      borderRadius: BorderRadius.circular(look.radius / 2),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          for (final candidate in _found)
            ListTile(
              key: ValueKey('mention ${candidate.label}'),
              dense: true,
              visualDensity: VisualDensity.compact,
              leading: Icon(candidate.folder ? Icons.folder_outlined : Icons.description_outlined, size: 18, color: look.muted),
              title: Text(candidate.label, maxLines: 1, overflow: TextOverflow.ellipsis, style: TextStyle(fontFamily: look.codeFont)),
              subtitle: candidate.detail == null ? null : Text(candidate.detail!, maxLines: 1, overflow: TextOverflow.ellipsis),
              onTap: () => widget.onPick(candidate),
            ),
        ],
      ),
    );
  }
}

/// How much of the account's usage allowance is used: the bucket that matters most with its refill, and a menu of every bucket the
/// agent reported. In the look's `warning` from the configured fraction used (a status), and in its `danger` once a bucket is
/// exhausted. The host decides whether to show it at all: an agent that reports no usage has no such control.
class UsagePill extends StatelessWidget {
  const UsagePill({super.key, required this.report, this.warnPermille = defaultWarnPermille, this.now});

  final UsageReport? report;
  final int warnPermille;

  /// Unix seconds; the clock's when null.
  final int? now;

  @override
  Widget build(BuildContext context) {
    final at = now ?? DateTime.now().millisecondsSinceEpoch ~/ 1000;
    final report = this.report;
    final headline = report == null ? null : headlineOf(report, at);
    final level = headline == null ? UsageLevel.calm : levelOf(headline, at, warnPermille: warnPermille);
    final text = report == null ? 'No usage yet' : pillText(report, at);
    return MenuAnchor(
      menuChildren: [UsageMenu(report: report, now: at)],
      builder: (context, controller, _) => InkWell(
        key: const Key('usage'),
        borderRadius: BorderRadius.circular(8),
        onTap: () => controller.isOpen ? controller.close() : controller.open(),
        child: Tooltip(
          message: 'Usage of the account behind this agent',
          child: ComposerPill(
            label: text,
            icon: Icons.data_usage,
            danger: level == UsageLevel.danger,
            status: level == UsageLevel.warn,
            maxWidth: 320,
          ),
        ),
      ),
    );
  }
}

/// What the usage control's menu lists: every bucket, the most pressing first, with what is known of it.
class UsageMenu extends StatelessWidget {
  const UsageMenu({super.key, required this.report, required this.now});

  final UsageReport? report;

  /// Unix seconds.
  final int now;

  @override
  Widget build(BuildContext context) {
    final look = context.chatLook;
    final report = this.report;
    final small = TextStyle(fontSize: 11.5, color: look.muted);
    final buckets = report == null ? const <UsageBucket>[] : ordered(report, now);
    final hit = report?.limit;
    return SelectionArea(
      child: ConstrainedBox(
        constraints: const BoxConstraints(minWidth: 240, maxWidth: 380),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              if (buckets.isEmpty) Text('Nothing reported yet', key: const Key('usage-empty'), style: small),
              for (final bucket in buckets)
                Padding(
                  key: Key('usage-bucket ${bucket.id}'),
                  padding: const EdgeInsets.only(bottom: 8),
                  child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
                    Text(
                      bucketLine(bucket, now),
                      style: TextStyle(
                        fontSize: 13,
                        color: switch (levelOf(bucket, now)) {
                          UsageLevel.danger => look.danger,
                          UsageLevel.warn => look.warning,
                          UsageLevel.calm => null,
                        },
                      ),
                    ),
                    for (final line in bucketDetails(bucket, now)) Text(line, style: small),
                  ]),
                ),
              if (hit != null)
                Padding(
                  padding: const EdgeInsets.only(bottom: 8),
                  child: Text(
                    [
                      'Last refusal',
                      if (hit.text != null) hit.text!,
                      if (hit.retryAfter != null) 'retry after ${spanText(hit.retryAfter!.inSeconds)}',
                      if (hit.resetsAt != null && hit.resetsAt! > now) 'back in ${spanText(hit.resetsAt! - now)}',
                    ].join(' · '),
                    style: small,
                  ),
                ),
              if (report?.account case final account?) Text('Account: $account', style: small),
              if (report != null && report.sources.isNotEmpty && !report.sources.contains('reported'))
                Text('The agent reports no figures: these are inferred from its errors', style: small),
            ],
          ),
        ),
      ),
    );
  }
}
