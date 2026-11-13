
import 'dart:async';

import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:flutter/material.dart';
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
  List<Widget> before = const [],
  List<Widget> after = const [],
  List<Widget> trailing = const [],
  List<Widget> plusItems = const [],
  double width = 900,
  GlobalKey<ComposerState>? key,
}) async {
  final controller = TextEditingController();
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
      before: before,
      after: after,
      trailing: trailing,
      plusItems: plusItems,
    ),
  )));
  return controller;
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

void main() {
  testWidgets('Send sends, and so does the keyboard\'s send', (tester) async {
    var sent = 0;
    await pump(tester, onSend: () => sent++);
    await tester.tap(find.byTooltip('Send'));
    await tester.enterText(find.byType(TextField), 'hello');
    await tester.testTextInput.receiveAction(TextInputAction.send);
    expect(sent, 2);
  });

  testWidgets('a disabled composer takes no text and sends nothing', (tester) async {
    await pump(tester, enabled: false);
    expect(tester.widget<TextField>(find.byType(TextField)).enabled, isFalse);
    expect(tester.widget<IconButton>(find.widgetWithIcon(IconButton, Icons.arrow_upward_rounded)).onPressed, isNull);
  });

  testWidgets('while the agent works, Send is Queue and Stop appears; starting shows a spinner instead', (tester) async {
    var stopped = false;
    await pump(tester, working: true, onStop: () => stopped = true);
    expect(find.byTooltip('Queue'), findsOneWidget);
    expect(find.byTooltip('Send'), findsNothing);
    await tester.tap(find.byTooltip('Stop'));
    expect(stopped, isTrue);
    await pump(tester, starting: true);
    expect(find.byType(CircularProgressIndicator), findsOneWidget);
    expect(find.byTooltip('Send'), findsNothing);
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

  testWidgets('the host\'s slots sit in the row: before and after the +, beside Send, in the + menu, above the message', (tester) async {
    await pump(
      tester,
      header: const Text('HEADER'),
      before: const [Text('BEFORE')],
      after: const [Text('AFTER')],
      trailing: const [Text('TRAILING')],
      plusItems: [MenuItemButton(onPressed: () {}, child: const Text('Host item'))],
    );
    expect(find.text('HEADER'), findsOneWidget);
    final x = {for (final label in ['BEFORE', 'AFTER', 'TRAILING']) label: tester.getTopLeft(find.text(label)).dx};
    final plus = tester.getTopLeft(find.byKey(const Key('composer-plus'))).dx;
    expect(x['BEFORE'], lessThan(plus));
    expect(x['AFTER'], greaterThan(plus));
    expect(x['TRAILING'], greaterThan(x['AFTER']!));
    expect(tester.getTopLeft(find.byTooltip('Send')).dx, greaterThan(x['TRAILING']!));
    await tester.tap(find.byKey(const Key('composer-plus')));
    await tester.pumpAndSettle();
    expect(find.text('Host item'), findsOneWidget);
    expect(find.text('Add files or images…'), findsNothing, reason: 'with no attachment host there is nothing to add to');
  });

  testWidgets('narrow, the controls wrap under the message and Send stays at the end', (tester) async {
    await pump(tester, width: 420, before: const [Text('BEFORE')], trailing: const [Text('TRAILING')]);
    expect(tester.takeException(), isNull);
    expect(find.text('TRAILING'), findsOneWidget);
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

    testWidgets('chips show what goes with the message, a note, a refusal, and take it back', (tester) async {
      final host = FakeAttachments()
        ..chips = [
          const ComposerChip(key: 'a', name: 'shot.png', detail: '12 KB', kind: ChipKind.image, thumbnail: [0]),
          const ComposerChip(key: 'b', name: 'big.log', detail: '9 MB', note: 'Larger than 1 MB: it goes as a link'),
          const ComposerChip(key: 'c', name: 'src', detail: 'Folder', kind: ChipKind.folder, note: 'The agent runs elsewhere', refused: true),
        ];
      await pump(tester, attachments: host);
      expect(find.text('12 KB'), findsOneWidget);
      expect(find.text('Larger than 1 MB: it goes as a link'), findsOneWidget);
      expect(find.text('Folder'), findsNothing, reason: 'a note replaces the detail');
      await tester.tap(find.byTooltip('Remove big.log'));
      expect(host.log, ['remove b']);
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
