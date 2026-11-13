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
  const ComposerPill({super.key, required this.label, this.icon, this.danger = false, this.status = false, this.enabled = true, this.maxWidth = 180});

  final String label;
  final IconData? icon;

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

  /// Shift+Tab goes to [Composer.onShiftTab]; Ctrl+V, or ⌘V on a Mac, pastes through [paste]; every other key goes on as usual.
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
        builder: (context, value, _) => CommandSuggestions(
          commands: widget.commands,
          typed: value.text,
          onPick: (command) {
            final text = '/${command.name} ';
            widget.controller.value = TextEditingValue(text: text, selection: TextSelection.collapsed(offset: text.length));
          },
        ),
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

/// The agent's commands matching what is being typed. Nothing runs until the person sends it.
class CommandSuggestions extends StatelessWidget {
  const CommandSuggestions({super.key, required this.commands, required this.typed, required this.onPick});

  static const int shown = 6;

  final List<AgentCommandInfo> commands;
  final String typed;
  final void Function(AgentCommandInfo command) onPick;

  @override
  Widget build(BuildContext context) {
    if (!typed.startsWith('/') || typed.contains(' ')) return const SizedBox.shrink();
    final wanted = typed.substring(1).toLowerCase();
    final found = commands.where((command) => command.name.toLowerCase().startsWith(wanted)).take(shown).toList();
    if (found.isEmpty) return const SizedBox.shrink();
    final look = context.chatLook;
    return Material(
      color: look.composer,
      borderRadius: BorderRadius.circular(look.radius / 2),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          for (final command in found)
            ListTile(
              dense: true,
              visualDensity: VisualDensity.compact,
              title: Text('/${command.name}', style: TextStyle(fontFamily: look.codeFont)),
              subtitle: command.description.isEmpty ? null : Text(command.description, maxLines: 1, overflow: TextOverflow.ellipsis),
              onTap: () => onPick(command),
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
