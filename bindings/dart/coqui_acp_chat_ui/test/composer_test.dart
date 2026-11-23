
import 'dart:async';
import 'dart:convert' show base64Decode;

import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:material_ui/material_ui.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fixtures.dart';

class FakeClipboard implements ClipboardReader {
  FakeClipboard({this.imageBytes, this.paths = const [], this.words});
  final Uint8List? imageBytes;
  final List<String> paths;
  final String? words;
  @override
  Future<Uint8List?> image() async => imageBytes;
  @override
  Future<List<String>> files() async => paths;
  @override
  Future<String?> text() async => words;
}

class FakeAttachments implements ComposerAttachments {
  final log = <String>[];
  @override
  bool canAttach = true;
  @override
  List<ComposerChip> chips = [];
  @override
  Future<List<String>> addFiles(List<PickedFile> files) async {
    log.add('files ${files.map((file) => file.name).join(',')}');
    return ['${files.first.name} cannot be read.'];
  }

  @override
  Future<List<String>> addPaths(List<String> paths) async {
    log.add('paths ${paths.join(',')}');
    return const [];
  }

  @override
  String? addImage(Uint8List bytes) {
    log.add('image ${bytes.length}');
    return null;
  }

  @override
  String? addText(String text) {
    log.add('text ${text.length}');
    return null;
  }

  @override
  String? addFolder(String path) {
    log.add('folder $path');
    return 'Folders are not taken here.';
  }

  @override
  void remove(Object key) => log.add('remove $key');
}

Future<TextEditingController> pump(
  WidgetTester tester, {
  VoidCallback? onSend,
  bool enabled = true,
  bool working = false,
  bool starting = false,
  VoidCallback? onStop,
  VoidCallback? onSteer,
  bool enterSteers = false,
  List<AgentCommandInfo> commands = const [],
  List<ComposerAction> actions = const [],
  String commandsTitle = 'Agent commands',
  String actionsTitle = 'Host actions',
  ComposerMentions? mentions,
  ComposerAttachments? attachments,
  ClipboardReader clipboard = const TextClipboard(),
  Future<List<PickedFile>> Function()? pickFiles,
  Future<String?> Function()? pickFolder,
  VoidCallback? onShiftTab,
  String? Function()? recall,
  Widget? header,
  Widget? primary,
  bool holdPrimary = false,
  List<Widget> below = const [],
  List<Widget> plusItems = const [],
  double width = 900,
  String text = '',
  bool centred = false,
  GlobalKey<ComposerState>? key,
}) async {
  final controller = TextEditingController(text: text);
  addTearDown(controller.dispose);
  tester.view.physicalSize = Size(width, 700);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(host(Align(
    alignment: Alignment.bottomCenter,
    child: Composer(
      key: key,
      controller: controller,
      onSend: onSend ?? () {},
      enabled: enabled,
      working: working,
      starting: starting,
      onStop: onStop,
      onSteer: onSteer,
      enterSteers: enterSteers,
      commands: commands,
      actions: actions,
      commandsTitle: commandsTitle,
      actionsTitle: actionsTitle,
      mentions: mentions,
      attachments: attachments,
      clipboard: clipboard,
      pickFiles: pickFiles,
      pickFolder: pickFolder,
      onShiftTab: onShiftTab,
      recall: recall,
      header: header,
      centred: centred,
      primary: primary,
      holdPrimary: holdPrimary,
      below: below,
      plusItems: plusItems,
    ),
  )));
  return controller;
}

/// A primary control with state of its own, which says when that state is made.
class _Counted extends StatefulWidget {
  const _Counted(this.made);
  final VoidCallback made;

  @override
  State<_Counted> createState() => _CountedState();
}

class _CountedState extends State<_Counted> {
  @override
  void initState() {
    super.initState();
    widget.made();
  }

  @override
  Widget build(BuildContext context) => const SizedBox(key: Key('counted'), width: 24, height: 24);
}

/// A host's mention source over a fixed table, by query; [slow] queries answer only once [release]d.
class _Mentions implements ComposerMentions {
  _Mentions(this.table, {this.refuse, this.slow = const {}});
  final Map<String, List<String>> table;
  final String? refuse;
  final Set<String> slow;
  final asked = <String>[];
  final picked = <String>[];
  final _gates = <String, Completer<void>>{};

  void release(String query) => (_gates[query] ??= Completer<void>()).complete();

  @override
  Future<List<MentionCandidate>> search(String query) async {
    asked.add(query);
    if (slow.contains(query)) await (_gates[query] ??= Completer<void>()).future;
    return [for (final label in table[query] ?? const <String>[]) MentionCandidate(label: label, detail: 'in the folder')];
  }

  @override
  String? pick(MentionCandidate candidate) {
    if (refuse != null) return refuse;
    picked.add(candidate.label);
    return null;
  }
}

/// A mention source that offers [all] whatever is typed, and records the key of each pick.
class _Groups implements ComposerMentions {
  _Groups(this.all, this.picked);
  final List<MentionCandidate> all;
  final List<Object?> picked;

  @override
  Future<List<MentionCandidate>> search(String query) async => all;

  @override
  String? pick(MentionCandidate candidate) {
    picked.add(candidate.key);
    return null;
  }
}
void main() {
  testWidgets('Send sends, and so does the keyboard\'s send', (tester) async {
    var sent = 0;
    await pump(tester, onSend: () => sent++);
    await tester.tap(find.byTooltip('Send'));
    await tester.enterText(find.byType(TextField), 'hello');
    await tester.testTextInput.receiveAction(TextInputAction.send);
    expect(sent, 2);
  });

  group('the one button at the right of the box', () {
    const mic = Icon(Icons.mic_none, key: Key('the-mic'));

    testWidgets('an empty message shows the host\'s primary control and no Send', (tester) async {
      await pump(tester, primary: mic);
      expect(find.byKey(const Key('the-mic')), findsOneWidget);
      expect(find.byTooltip('Send'), findsNothing);
    });

    testWidgets('typing puts the primary control away for Send, and emptying the message brings it back', (tester) async {
      var sent = 0;
      final controller = await pump(tester, primary: mic, onSend: () => sent++);
      await tester.enterText(find.byType(TextField), 'hello');
      await tester.pump();
      expect(find.byKey(const Key('the-mic')), findsNothing);
      await tester.tap(find.byTooltip('Send'));
      expect(sent, 1);
      controller.clear();
      await tester.pump();
      expect(find.byKey(const Key('the-mic')), findsOneWidget);
      expect(find.byTooltip('Send'), findsNothing);
      // Spaces are no message.
      controller.text = '   ';
      await tester.pump();
      expect(find.byKey(const Key('the-mic')), findsOneWidget);
    });

    testWidgets('an attachment with no text is a message: Send shows', (tester) async {
      final host = FakeAttachments()..chips = const [ComposerChip(key: 'a', name: 'shot.png', detail: '12 KB', kind: ChipKind.image, thumbnail: [0])];
      await pump(tester, primary: mic, attachments: host);
      expect(find.byTooltip('Send'), findsOneWidget);
      expect(find.byKey(const Key('the-mic')), findsNothing);
    });

    testWidgets('the primary control is put away, not rebuilt: what it holds outlives the typing', (tester) async {
      var made = 0;
      final controller = await pump(tester, primary: _Counted(() => made++));
      expect(made, 1);
      controller.text = 'hello';
      await tester.pump();
      expect(find.byKey(const Key('counted')), findsNothing, reason: 'not drawn');
      controller.clear();
      await tester.pump();
      expect(find.byKey(const Key('counted')), findsOneWidget);
      expect(made, 1, reason: 'the same state throughout');
    });

    testWidgets('holdPrimary keeps the primary control beside Send while it is mid-action', (tester) async {
      await pump(tester, primary: mic, holdPrimary: true, text: 'hello');
      expect(find.byKey(const Key('the-mic')), findsOneWidget);
      expect(find.byTooltip('Send'), findsOneWidget);
      expect(tester.getTopLeft(find.byTooltip('Send')).dx, lessThan(tester.getTopLeft(find.byKey(const Key('the-mic'))).dx));
    });

    testWidgets('the primary control sits at the right end of the box, + at the left, the text between', (tester) async {
      await pump(tester, primary: mic);
      final box = tester.getRect(find.byKey(const Key('composer')));
      final plus = tester.getRect(find.byKey(const Key('composer-plus')));
      final field = tester.getRect(find.byType(TextField));
      final primary = tester.getRect(find.byKey(const Key('the-mic')));
      expect(plus.left - box.left, lessThan(16));
      expect(box.right - primary.right, lessThan(16));
      expect(field.left, greaterThan(plus.right - 1));
      expect(field.right, lessThan(primary.left + 1));
    });

    testWidgets('while a turn runs and nothing is typed, Stop shows beside the primary control, and neither Queue nor Steer', (tester) async {
      var stopped = 0;
      await pump(tester, primary: mic, working: true, onSteer: () {}, onStop: () => stopped++);
      expect(find.byKey(const Key('the-mic')), findsOneWidget);
      expect(find.byKey(const Key('composer-queue')), findsNothing);
      expect(find.byKey(const Key('composer-steer')), findsNothing);
      await tester.tap(find.byTooltip('Stop'));
      expect(stopped, 1);
    });

    testWidgets('while a turn runs and something is typed, Queue, Steer and Stop show and the primary control is away', (tester) async {
      await pump(tester, primary: mic, working: true, onSteer: () {}, onStop: () {}, text: 'next');
      expect(find.byKey(const Key('the-mic')), findsNothing);
      expect(find.byKey(const Key('composer-queue')), findsOneWidget);
      expect(find.byKey(const Key('composer-steer')), findsOneWidget);
      expect(find.byTooltip('Stop'), findsOneWidget);
    });
  });

  testWidgets('a disabled composer takes no text and sends nothing', (tester) async {
    await pump(tester, enabled: false);
    expect(tester.widget<TextField>(find.byType(TextField)).enabled, isFalse);
    expect(tester.widget<IconButton>(find.widgetWithIcon(IconButton, Icons.keyboard_return)).onPressed, isNull);
  });

  testWidgets('while the agent works, a typed message is queued and Stop appears; starting shows a spinner instead', (tester) async {
    var stopped = false;
    await pump(tester, working: true, onStop: () => stopped = true);
    expect(find.byTooltip('Queue'), findsNothing, reason: 'nothing is typed to queue');
    await tester.enterText(find.byType(TextField), 'next');
    await tester.pump();
    expect(find.byTooltip('Queue'), findsOneWidget);
    expect(find.byTooltip('Send'), findsNothing);
    await tester.tap(find.byTooltip('Stop'));
    expect(stopped, isTrue);
    await pump(tester, starting: true);
    expect(find.byType(CircularProgressIndicator), findsOneWidget);
    expect(find.byTooltip('Send'), findsNothing);
  });

  group('steering while the agent works', () {
    testWidgets('Queue, Steer and Stop sit together, and each tooltip says its key', (tester) async {
      final log = <String>[];
      await pump(tester, working: true, onSend: () => log.add('queue'), onSteer: () => log.add('steer'), onStop: () => log.add('stop'), text: 'next');
      await tester.tap(find.byKey(const Key('composer-steer')));
      await tester.tap(find.byKey(const Key('composer-queue')));
      await tester.tap(find.byTooltip('Stop'));
      expect(log, ['steer', 'queue', 'stop']);
      expect(find.byTooltip('Queue: send this when the turn ends (Enter)'), findsOneWidget);
      expect(find.byTooltip('Steer: stop the turn and send this first (Alt+Enter)'), findsOneWidget);
    });

    testWidgets('Enter queues and Alt+Enter steers by default', (tester) async {
      final log = <String>[];
      await pump(tester, working: true, onSend: () => log.add('queue'), onSteer: () => log.add('steer'));
      await tester.tap(find.byType(TextField));
      await tester.enterText(find.byType(TextField), 'hello');
      await tester.testTextInput.receiveAction(TextInputAction.send);
      await tester.tap(find.byType(TextField)); // submitting leaves the field
      await tester.sendKeyDownEvent(LogicalKeyboardKey.altLeft);
      await tester.sendKeyDownEvent(LogicalKeyboardKey.enter);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.enter);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.altLeft);
      expect(log, ['queue', 'steer']);
    });

    testWidgets('with enterSteers it is the other way round, and the tooltips follow', (tester) async {
      final log = <String>[];
      await pump(tester, working: true, enterSteers: true, onSend: () => log.add('queue'), onSteer: () => log.add('steer'), text: 'next');
      await tester.tap(find.byType(TextField));
      await tester.enterText(find.byType(TextField), 'hello');
      await tester.testTextInput.receiveAction(TextInputAction.send);
      await tester.tap(find.byType(TextField)); // submitting leaves the field
      await tester.sendKeyDownEvent(LogicalKeyboardKey.altLeft);
      await tester.sendKeyDownEvent(LogicalKeyboardKey.enter);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.enter);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.altLeft);
      expect(log, ['steer', 'queue']);
      expect(find.byTooltip('Steer: stop the turn and send this first (Enter)'), findsOneWidget);
      expect(find.byTooltip('Queue: send this when the turn ends (Alt+Enter)'), findsOneWidget);
    });

    testWidgets('with no turn running, or no steer given, Enter sends and Steer is not offered', (tester) async {
      final log = <String>[];
      await pump(tester, enterSteers: true, onSend: () => log.add('send'), onSteer: () => log.add('steer'));
      expect(find.byKey(const Key('composer-steer')), findsNothing);
      await tester.enterText(find.byType(TextField), 'hello');
      await tester.testTextInput.receiveAction(TextInputAction.send);
      expect(log, ['send']);
      await pump(tester, working: true, text: 'next');
      expect(find.byKey(const Key('composer-steer')), findsNothing);
      expect(find.byTooltip('Queue'), findsOneWidget);
    });
  });

  testWidgets('a / lists the commands that match, and choosing one writes it with a space', (tester) async {
    final controller = await pump(tester, commands: const [AgentCommandInfo('compact', 'Make room'), AgentCommandInfo('review', ''), AgentCommandInfo('cost', 'Show cost')]);
    expect(find.byType(ListTile), findsNothing);
    await tester.enterText(find.byType(TextField), '/co');
    await tester.pump();
    expect(find.text('/compact'), findsOneWidget);
    expect(find.text('/cost'), findsOneWidget);
    expect(find.text('/review'), findsNothing);
    await tester.tap(find.text('/compact'));
    await tester.pump();
    expect(controller.text, '/compact ');
    expect(find.byType(ListTile), findsNothing, reason: 'a typed space ends the suggestions');
  });

  testWidgets('the host\'s slots: the header in the box, the primary control at its right, the choices under it, the entries in the + menu', (tester) async {
    await pump(
      tester,
      header: const Text('HEADER'),
      primary: const Text('PRIMARY'),
      below: const [Text('BELOW')],
      plusItems: [MenuItemButton(onPressed: () {}, child: const Text('Host item'))],
    );
    final box = tester.getRect(find.byKey(const Key('composer')));
    expect(box.contains(tester.getCenter(find.text('HEADER'))), isTrue);
    expect(box.contains(tester.getCenter(find.text('PRIMARY'))), isTrue);
    expect(tester.getTopLeft(find.text('PRIMARY')).dx, greaterThan(tester.getTopLeft(find.byKey(const Key('composer-plus'))).dx));
    expect(tester.getTopLeft(find.text('BELOW')).dy, greaterThan(box.bottom - 1), reason: 'the line is under the box');
    await tester.tap(find.byKey(const Key('composer-plus')));
    await tester.pumpAndSettle();
    expect(find.text('Host item'), findsOneWidget);
    expect(find.text('Add files or images…'), findsNothing, reason: 'with no attachment host there is nothing to add to');
  });

  group('a centred box', () {
    for (final width in [900.0, 360.0]) {
      testWidgets('keeps +, the text and the primary control on one row at ${width.toInt()} px, in a taller box', (tester) async {
        await pump(tester, centred: true, width: width, primary: const Icon(Icons.mic_none, key: Key('the-mic')));
        final box = tester.getRect(find.byKey(const Key('composer')));
        final plus = tester.getRect(find.byKey(const Key('composer-plus')));
        final field = tester.getRect(find.byType(TextField));
        final mic = tester.getRect(find.byKey(const Key('the-mic')));
        expect(box.height, greaterThanOrEqualTo(112), reason: 'taller than the docked box');
        expect((field.center.dy - plus.center.dy).abs(), lessThan(12), reason: '+ is beside the text');
        expect((field.center.dy - mic.center.dy).abs(), lessThan(14), reason: 'the primary control is beside the text');
        expect(box.bottom - field.bottom, lessThan(16), reason: 'the row is at the bottom of the box');
        expect(plus.right, lessThanOrEqualTo(field.left + 1));
        expect(mic.left, greaterThanOrEqualTo(field.right - 1));
      });
    }

    testWidgets('what is typed grows the box upward: the row stays at the bottom', (tester) async {
      final controller = await pump(tester, centred: true);
      final before = tester.getRect(find.byKey(const Key('composer')));
      controller.text = List.filled(6, 'a line of text').join('\n');
      await tester.pump();
      final after = tester.getRect(find.byKey(const Key('composer')));
      expect(after.height, greaterThan(before.height));
      expect(after.bottom, before.bottom, reason: 'the box grows toward the top');
      expect(tester.getRect(find.byKey(const Key('composer-plus'))).bottom, closeTo(after.bottom - 6, 12), reason: '+ stays on the bottom line');
    });
  });

  group('the line under the box', () {
    // The test font draws every glyph a 12 px square, so these are far wider than they would be.
    const labels = ['Opus', 'High', 'Bypass', 'Work', 'Local'];
    List<Widget> choices() => [for (final label in labels) ComposerPill(label: label, icon: Icons.tune)];

    testWidgets('wide, it is right-aligned on one line, as plain text with no icon or chevron', (tester) async {
      await pump(tester, below: choices());
      final box = tester.getRect(find.byKey(const Key('composer')));
      final line = tester.getRect(find.byKey(const Key('composer-below')));
      expect(find.text('Opus'), findsOneWidget);
      expect(find.byIcon(Icons.tune), findsNothing);
      expect(find.byIcon(Icons.expand_more), findsNothing);
      final first = tester.getRect(find.text('Opus')), last = tester.getRect(find.text('Local'));
      expect(first.top, last.top, reason: 'one line');
      expect(first.left, greaterThan(box.left + box.width / 3), reason: 'pushed to the right');
      expect(box.right - last.right, lessThan(24));
      expect(line.top, greaterThanOrEqualTo(box.bottom));
    });

    testWidgets('at a phone\'s width it starts at the left and wraps, with nothing overflowing', (tester) async {
      await pump(tester, width: 360, below: choices());
      expect(tester.takeException(), isNull);
      final box = tester.getRect(find.byKey(const Key('composer')));
      final first = tester.getRect(find.text('Opus')), last = tester.getRect(find.text('Local'));
      expect(last.top, greaterThan(first.top), reason: 'on more than one line');
      expect(first.left - box.left, lessThan(24), reason: 'from the left');
      for (final label in labels) {
        expect(tester.getRect(find.text(label)).right, lessThanOrEqualTo(box.right));
      }
    });

    testWidgets('a pill outside it keeps its icon and chevron', (tester) async {
      await tester.pumpWidget(host(const ComposerPill(label: 'Manual', icon: Icons.tune)));
      expect(find.byIcon(Icons.tune), findsOneWidget);
      expect(find.byIcon(Icons.expand_more), findsOneWidget);
    });

    testWidgets('with nothing to put there there is no line', (tester) async {
      await pump(tester);
      expect(find.byKey(const Key('composer-below')), findsNothing);
    });
  });

  testWidgets('narrow, the box still holds +, the text and the button, and a long hint stays on a line', (tester) async {
    await pump(tester, width: 320, primary: const Icon(Icons.mic_none), below: const [Text('BELOW')]);
    expect(tester.takeException(), isNull);
    expect(find.byKey(const Key('composer-plus')), findsOneWidget);
    expect(find.byType(TextField), findsOneWidget);
    expect(find.byIcon(Icons.mic_none), findsOneWidget);
  });

  testWidgets('Shift+Tab goes to the host, and only when it has asked for it', (tester) async {
    var pressed = 0;
    await pump(tester, onShiftTab: () => pressed++);
    await tester.tap(find.byType(TextField));
    await tester.sendKeyDownEvent(LogicalKeyboardKey.shiftLeft);
    await tester.sendKeyEvent(LogicalKeyboardKey.tab);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.shiftLeft);
    expect(pressed, 1);
    // Shift+Tab with nothing to do keeps its own meaning.
    await pump(tester);
    await tester.tap(find.byType(TextField));
    await tester.sendKeyDownEvent(LogicalKeyboardKey.shiftLeft);
    await tester.sendKeyEvent(LogicalKeyboardKey.tab);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.shiftLeft);
    expect(pressed, 1);
  });

  testWidgets('Up in an empty message brings back the last one sent, and moves the cursor anywhere else', (tester) async {
    final controller = await pump(tester, recall: () => 'what I said before');
    await tester.tap(find.byType(TextField));
    await tester.sendKeyEvent(LogicalKeyboardKey.arrowUp);
    await tester.pump();
    expect(controller.text, 'what I said before');
    expect(controller.selection, const TextSelection.collapsed(offset: 18));
    controller.text = 'second line';
    await tester.pump();
    await tester.sendKeyEvent(LogicalKeyboardKey.arrowUp);
    await tester.pump();
    expect(controller.text, 'second line');
  });

  testWidgets('Up in an empty message with nothing sent leaves it empty', (tester) async {
    final controller = await pump(tester, recall: () => null);
    await tester.tap(find.byType(TextField));
    await tester.sendKeyEvent(LogicalKeyboardKey.arrowUp);
    expect(controller.text, isEmpty);
  });

  testWidgets('a placeholder longer than the field stays on one line', (tester) async {
    final controller = TextEditingController();
    addTearDown(controller.dispose);
    tester.view.physicalSize = const Size(320, 700);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    const hint = 'A placeholder that is far too long to fit the width of a phone in one line of text';
    await tester.pumpWidget(host(Align(alignment: Alignment.bottomCenter, child: Composer(controller: controller, onSend: () {}, hint: hint))));
    expect(tester.getSize(find.text(hint)).height, lessThan(30));
  });

  group('the host\'s actions beside the agent\'s commands', () {
    testWidgets('with actions, / lists two titled groups, and an action runs when chosen and sends nothing', (tester) async {
      var ran = 0;
      var sent = 0;
      final controller = await pump(
        tester,
        onSend: () => sent++,
        commands: const [AgentCommandInfo('compact', 'Make room')],
        actions: [ComposerAction(name: 'settings', description: 'Open Settings', run: () => ran++)],
        actionsTitle: 'Canoa actions',
      );
      await tester.enterText(find.byType(TextField), '/');
      await tester.pump();
      expect(find.text('Agent commands'), findsOneWidget);
      expect(find.text('Canoa actions'), findsOneWidget);
      expect(
        tester.getTopLeft(find.byKey(const Key('commands-group'))).dy,
        lessThan(tester.getTopLeft(find.byKey(const Key('actions-group'))).dy),
      );
      expect(find.text('/compact'), findsOneWidget);
      expect(find.text('/settings'), findsOneWidget);
      await tester.tap(find.text('/settings'));
      await tester.pump();
      expect(ran, 1);
      expect(sent, 0);
      expect(controller.text, isEmpty, reason: 'the typed slash is taken back');
    });

    testWidgets('actions with a group are listed under its heading, each group together, and a pick runs it', (tester) async {
      final ran = <String>[];
      await pump(
        tester,
        actions: [
          ComposerAction(name: 'settings', description: 'Open Settings', run: () => ran.add('settings')),
          ComposerAction(name: 'review', description: 'A skill', group: 'Skills', run: () => ran.add('review')),
          ComposerAction(name: 'github', description: 'An MCP server', group: 'MCP servers', run: () => ran.add('github')),
          ComposerAction(name: 'setup', description: 'A skill again', group: 'Skills', run: () => ran.add('setup')),
        ],
        actionsTitle: 'Canoa actions',
      );
      await tester.enterText(find.byType(TextField), '/');
      await tester.pump();
      expect(find.text('Canoa actions'), findsOneWidget);
      expect(find.text('Skills'), findsOneWidget, reason: 'one heading for the two skills, which are not split by another group');
      expect(find.text('MCP servers'), findsOneWidget);
      final skills = tester.getTopLeft(find.byKey(const Key('group Skills'))).dy;
      expect(tester.getTopLeft(find.text('/setup')).dy, greaterThan(skills));
      expect(tester.getTopLeft(find.text('/setup')).dy, lessThan(tester.getTopLeft(find.byKey(const Key('group MCP servers'))).dy));
      await tester.tap(find.text('/github'));
      await tester.pump();
      expect(ran, ['github']);
    });
    testWidgets('a group with no match has no title, and with no actions the list has none at all', (tester) async {
      await pump(
        tester,
        commands: const [AgentCommandInfo('compact', '')],
        actions: [ComposerAction(name: 'settings', description: '', run: () {})],
      );
      await tester.enterText(find.byType(TextField), '/se');
      await tester.pump();
      expect(find.byKey(const Key('commands-group')), findsNothing);
      expect(find.byKey(const Key('actions-group')), findsOneWidget);
      await pump(tester, commands: const [AgentCommandInfo('compact', '')]);
      await tester.enterText(find.byType(TextField), '/');
      await tester.pump();
      expect(find.text('/compact'), findsOneWidget);
      expect(find.text('Agent commands'), findsNothing);
    });
  });

  group('@ mentions', () {
    test('the @ being typed is found at the start or after a space, up to the caret', () {
      expect(mentionAt('@sr', 3), (start: 0, query: 'sr'));
      expect(mentionAt('read @src/a', 11), (start: 5, query: 'src/a'));
      expect(mentionAt('read @src/a and', 11), (start: 5, query: 'src/a'));
      expect(mentionAt('mail me@home', 12), isNull, reason: 'an @ inside a word is not a mention');
      expect(mentionAt('read @a b', 9), isNull, reason: 'a space ends it');
      expect(mentionAt('hello', 5), isNull);
    });

    testWidgets('an @ lists what the host offers; choosing one writes @label and tells the host', (tester) async {
      final source = _Mentions({
        '': ['README.md', 'src/main.dart'],
        'ma': ['src/main.dart'],
      });
      final controller = await pump(tester, mentions: source);
      await tester.enterText(find.byType(TextField), 'look at @ma');
      await tester.pump();
      await tester.pump();
      expect(source.asked, ['ma']);
      expect(find.byKey(const ValueKey('mention src/main.dart')), findsOneWidget);
      expect(find.byKey(const ValueKey('mention README.md')), findsNothing);
      await tester.tap(find.byKey(const ValueKey('mention src/main.dart')));
      await tester.pump();
      expect(source.picked, ['src/main.dart']);
      expect(controller.text, 'look at @src/main.dart ');
      expect(find.byKey(const Key('mention-suggestions')), findsNothing, reason: 'the space ended the mention');
    });

    testWidgets('candidates with a group are listed under its heading, an icon of their own, and a pick reaches the host with its key', (tester) async {
      final picked = <Object?>[];
      final source = _Groups(
        [
          const MentionCandidate(label: 'src/main.dart'),
          const MentionCandidate(label: 'Fix login', group: 'Conversations', icon: Icons.chat_bubble_outline, key: 'chat-1'),
          const MentionCandidate(label: 'Plan release', group: 'Conversations', key: 'chat-2'),
        ],
        picked,
      );
      final controller = await pump(tester, mentions: source);
      await tester.enterText(find.byType(TextField), '@');
      await tester.pump();
      await tester.pump();
      expect(find.byKey(const Key('mention-group Conversations')), findsOneWidget, reason: 'one heading for the two');
      expect(find.byIcon(Icons.chat_bubble_outline), findsOneWidget);
      expect(
        tester.getTopLeft(find.byKey(const ValueKey('mention src/main.dart'))).dy,
        lessThan(tester.getTopLeft(find.byKey(const Key('mention-group Conversations'))).dy),
      );
      await tester.tap(find.byKey(const ValueKey('mention Conversations Fix login')));
      await tester.pump();
      expect(picked, ['chat-1']);
      expect(controller.text, '@Fix login ');
    });
    testWidgets('a choice the host refuses is said and leaves the text as it was', (tester) async {
      final source = _Mentions({'': ['../secret']}, refuse: 'That is outside the folder.');
      final controller = await pump(tester, mentions: source);
      await tester.enterText(find.byType(TextField), '@');
      await tester.pump();
      await tester.pump();
      await tester.tap(find.byKey(const ValueKey('mention ../secret')));
      await tester.pump();
      expect(find.text('That is outside the folder.'), findsOneWidget);
      expect(controller.text, '@');
    });

    testWidgets('an answer to an older query does not replace the newer one', (tester) async {
      final source = _Mentions({'a': ['slow.txt'], 'ab': ['fast.txt']}, slow: {'a'});
      await pump(tester, mentions: source);
      await tester.enterText(find.byType(TextField), '@a');
      await tester.pump();
      await tester.enterText(find.byType(TextField), '@ab');
      await tester.pump();
      await tester.pump();
      source.release('a');
      await tester.pump();
      await tester.pump();
      expect(find.byKey(const ValueKey('mention fast.txt')), findsOneWidget);
      expect(find.byKey(const ValueKey('mention slow.txt')), findsNothing);
    });

    testWidgets('with no source an @ stays text', (tester) async {
      final controller = await pump(tester);
      await tester.enterText(find.byType(TextField), '@x');
      await tester.pump();
      expect(find.byType(ListTile), findsNothing);
      expect(controller.text, '@x');
    });
  });

  group('attachments', () {
    testWidgets('the + menu adds files by the host\'s picker and says what went wrong', (tester) async {
      final host = FakeAttachments();
      await pump(
        tester,
        attachments: host,
        pickFiles: () async => [(name: 'notes.txt', path: '/home/ana/notes.txt', mimeType: null, read: () async => Uint8List(0))],
      );
      await tester.tap(find.byKey(const Key('composer-plus')));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('add-files')));
      await tester.pumpAndSettle();
      expect(host.log, ['files notes.txt']);
      expect(find.text('notes.txt cannot be read.'), findsOneWidget);
    });

    testWidgets('a folder is added by its path, and a refusal is said', (tester) async {
      final host = FakeAttachments();
      await pump(tester, attachments: host, pickFolder: () async => '/home/ana/work');
      await tester.tap(find.byKey(const Key('composer-plus')));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('add-folder')));
      await tester.pumpAndSettle();
      expect(host.log, ['folder /home/ana/work']);
      expect(find.text('Folders are not taken here.'), findsOneWidget);
    });

    testWidgets('an entry with no picker is disabled, and + is off while nothing can be attached', (tester) async {
      final host = FakeAttachments();
      await pump(tester, attachments: host);
      await tester.tap(find.byKey(const Key('composer-plus')));
      await tester.pumpAndSettle();
      expect(tester.widget<MenuItemButton>(find.byKey(const Key('add-files'))).onPressed, isNull);
      expect(tester.widget<MenuItemButton>(find.byKey(const Key('add-folder'))).onPressed, isNull);
      host.canAttach = false;
      await pump(tester, attachments: host);
      expect(tester.widget<IconButton>(find.byKey(const Key('composer-plus'))).onPressed, isNull);
    });

    testWidgets('an entry of the host keeps + on while nothing can be attached, and the entries that attach are off', (tester) async {
      final host = FakeAttachments()..canAttach = false;
      await pump(tester, attachments: host, pickFiles: () async => const [], plusItems: [MenuItemButton(onPressed: () {}, child: const Text('Host item'))]);
      await tester.tap(find.byKey(const Key('composer-plus')));
      await tester.pumpAndSettle();
      expect(find.text('Host item'), findsOneWidget);
      expect(tester.widget<MenuItemButton>(find.byKey(const Key('add-files'))).onPressed, isNull);
      expect(tester.widget<MenuItemButton>(find.byKey(const Key('paste'))).onPressed, isNull);
    });

    testWidgets('pasting an image, copied files or a long text goes to the host; a short text stays text', (tester) async {
      final host = FakeAttachments();
      final key = GlobalKey<ComposerState>();
      final controller = await pump(tester, attachments: host, key: key, clipboard: FakeClipboard(imageBytes: Uint8List.fromList([1, 2, 3])));
      await key.currentState!.paste();
      await pump(tester, attachments: host, key: key, clipboard: FakeClipboard(paths: const ['/home/ana/a.png', '/home/ana/b.txt']));
      await key.currentState!.paste();
      await pump(tester, attachments: host, key: key, clipboard: FakeClipboard(words: 'x' * (Composer.defaultPasteAsFile + 1)));
      await key.currentState!.paste();
      expect(host.log, ['image 3', 'paths /home/ana/a.png,/home/ana/b.txt', 'text ${Composer.defaultPasteAsFile + 1}']);
      expect(controller.text, isEmpty);
      final short = await pump(tester, attachments: host, key: key, clipboard: FakeClipboard(words: 'short'));
      await key.currentState!.paste();
      expect(short.text, 'short');
    });

    testWidgets('a short paste goes in at the cursor, over a selection', (tester) async {
      final key = GlobalKey<ComposerState>();
      final controller = await pump(tester, key: key, clipboard: FakeClipboard(words: 'XY'));
      controller.value = const TextEditingValue(text: 'abcdef', selection: TextSelection(baseOffset: 2, extentOffset: 4));
      await key.currentState!.paste();
      expect(controller.text, 'abXYef');
      expect(controller.selection, const TextSelection.collapsed(offset: 4));
    });

    testWidgets('a long paste with no attachment host stays text', (tester) async {
      final key = GlobalKey<ComposerState>();
      final controller = await pump(tester, key: key, clipboard: FakeClipboard(words: 'x' * 9000));
      await key.currentState!.paste();
      expect(controller.text.length, 9000);
    });

    testWidgets('cards show what goes with the message, a note, a refusal, and take it back', (tester) async {
      final host = FakeAttachments()
        ..chips = [
          const ComposerChip(key: 'a', name: 'shot.png', detail: '12 KB', kind: ChipKind.image, thumbnail: [0]),
          const ComposerChip(key: 'b', name: 'big.log', detail: '9 MB', note: 'Larger than 1 MB: it goes as a link'),
          const ComposerChip(key: 'c', name: 'src', detail: 'Folder', kind: ChipKind.folder, note: 'The agent runs elsewhere', refused: true),
        ];
      await pump(tester, attachments: host);
      expect(find.text('big.log'), findsOneWidget);
      expect(find.text('Larger than 1 MB: it goes as a link'), findsOneWidget);
      expect(find.text('Folder'), findsNothing, reason: 'a note replaces the detail');
      await tester.tap(find.byTooltip('Remove big.log'));
      expect(host.log, ['remove b']);
    });

    // A 1x1 PNG.
    final png = base64Decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==');

    testWidgets('an image is a picture card above the text, and its x takes it back', (tester) async {
      final host = FakeAttachments()..chips = [ComposerChip(key: 'a', name: 'shot.png', detail: '12 KB', kind: ChipKind.image, thumbnail: png)];
      await pump(tester, attachments: host, primary: const Icon(Icons.mic_none), text: 'look at this');
      final card = tester.getRect(find.byKey(const ValueKey('attachment shot.png')));
      final box = tester.getRect(find.byKey(const Key('composer')));
      expect(find.byType(Image), findsOneWidget, reason: 'the picture itself');
      expect(card.size, const Size.square(AttachmentChips.cardSize + 2), reason: 'a card, not a one-line chip');
      expect(box.contains(card.center), isTrue, reason: 'inside the box');
      expect(card.bottom, lessThan(tester.getTopLeft(find.byType(TextField)).dy), reason: 'above the text');
      expect(find.text('shot.png'), findsNothing, reason: 'a picture says nothing more than its tooltip');
      await tester.tap(find.byTooltip('Remove shot.png'));
      expect(host.log, ['remove a']);
    });

    testWidgets('a picture with a note says it in a line under the cards, a refused one outlined and marked', (tester) async {
      final host = FakeAttachments()..chips = [ComposerChip(key: 'a', name: 'shot.png', detail: '12 KB', kind: ChipKind.image, thumbnail: png, note: 'Too large to attach', refused: true)];
      await pump(tester, attachments: host);
      expect(find.text('shot.png: Too large to attach'), findsOneWidget);
      expect(find.byTooltip('shot.png\nToo large to attach'), findsOneWidget);
      expect(find.byIcon(Icons.error_outline), findsOneWidget);
      expect(tester.getTopLeft(find.text('shot.png: Too large to attach')).dy, lessThan(tester.getTopLeft(find.byType(TextField)).dy), reason: 'above the text');
    });
  });

  testWidgets('a pill draws a mark of the host where an icon would go, in its size and colour', (tester) async {
    await tester.pumpWidget(host(ComposerPill(label: 'Claude', leading: Builder(builder: (context) => Icon(Icons.star, size: IconTheme.of(context).size)))));
    expect(find.text('Claude'), findsOneWidget);
    expect(tester.widget<Icon>(find.byIcon(Icons.star)).size, 16);
  });

  group('usage', () {
    const report = UsageReport(
      supported: true,
      revision: 1,
      buckets: [UsageBucket(id: 'five_hour', label: '5 hours', used: 900, limit: 1000)],
    );

    testWidgets('the pill shows the bucket that matters and the menu lists them', (tester) async {
      await tester.pumpWidget(host(UsagePill(report: report, now: 1000)));
      expect(find.byKey(const Key('usage')), findsOneWidget);
      await tester.tap(find.byKey(const Key('usage')));
      await tester.pumpAndSettle();
      expect(find.byType(UsageMenu), findsOneWidget);
      expect(find.byKey(const Key('usage-bucket five_hour')), findsOneWidget);
    });

    testWidgets('with no report it says so', (tester) async {
      await pumpUsage(tester, null);
      expect(find.text('No usage yet'), findsOneWidget);
      await tester.tap(find.byKey(const Key('usage')));
      await tester.pumpAndSettle();
      expect(find.byKey(const Key('usage-empty')), findsOneWidget);
    });
  });
}

Future<void> pumpUsage(WidgetTester tester, UsageReport? report) => tester.pumpWidget(host(UsagePill(report: report, now: 1000)));
