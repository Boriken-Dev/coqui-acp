import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fixtures.dart';

String shape(List<TranscriptItem> items) => items
    .map(
      (item) => switch (item) {
        EntryItem(:final entry) => entry.role.name,
        BurstItem(:final calls) => 'burst${calls.length}',
        WorkItem(:final items) => 'work[${shape(items)}]',
      },
    )
    .join(' ');

void main() {
  group('foldTranscript', () {
    test('verbose leaves every entry as it is', () {
      expect(shape(foldTranscript(worked(), density: ChatDensity.verbose)), 'user thought tool tool agent');
    });

    test('thinking folds a run of finished tool calls into one burst and keeps the thought', () {
      expect(shape(foldTranscript(worked(), density: ChatDensity.thinking)), 'user thought burst2 agent');
    });

    test('normal folds the finished turn from its first piece of work to its last, and leaves the answer', () {
      final items = foldTranscript(worked(), density: ChatDensity.normal);
      expect(shape(items), 'user work[thought burst2] agent');
      final work = items[1] as WorkItem;
      // From the person's message to the last tool call.
      expect(work.duration, const Duration(seconds: 40));
      expect(work.tools.map((call) => call.id), ['t1', 't2']);
    });

    test('a turn still running folds its work so far into a live block that keeps the thoughts', () {
      final items = foldTranscript(worked().sublist(0, 4), density: ChatDensity.normal, working: true);
      expect(shape(items), 'user work[thought burst2]');
      expect((items[1] as WorkItem).running, isTrue);
    });

    test('a running turn leaves a call that runs whole after the live block, and what the agent said after the work outside', () {
      final entries = [
        user('go', 0),
        thought('hm', 1),
        toolEntry(tool('a', kind: 'read'), 2),
        toolEntry(tool('b', kind: 'execute', status: 'in_progress'), 3),
      ];
      expect(shape(foldTranscript(entries, density: ChatDensity.normal, working: true)), 'user work[thought burst1] tool');
      final talking = [user('go', 0), toolEntry(tool('a', kind: 'read'), 2), agent('Looking', 3)];
      expect(shape(foldTranscript(talking, density: ChatDensity.normal, working: true)), 'user work[burst1] agent');
    });

    test('a running turn with no work yet has no block, and Thinking shows its work as it is', () {
      expect(shape(foldTranscript([user('go', 0)], density: ChatDensity.normal, working: true)), 'user');
      expect(shape(foldTranscript(worked().sublist(0, 4), density: ChatDensity.thinking, working: true)), 'user thought burst2');
    });

    test('a turn of thoughts alone folds when it ends', () {
      final entries = [user('hm', 0), thought('thinking', 1), agent('Nothing to do.', 9)];
      expect(shape(foldTranscript(entries, density: ChatDensity.normal)), 'user work[thought] agent');
    });

    test('the duration is the turn itself, from its start to its end', () {
      // One call a second in, the turn took 45 s: the entries alone would have said 1 s.
      final entries = [
        ChatEntry(MessageRole.user, 'go', at: t0, turn: 7),
        ChatEntry(MessageRole.tool, 'A tool', at: t0.add(const Duration(seconds: 1)), tool: tool('a', kind: 'execute'), turn: 7),
        ChatEntry(MessageRole.agent, 'ok', at: t0.add(const Duration(seconds: 45)), turn: 7),
      ];
      final turns = [TurnSpan(id: 7, startedAt: t0, endedAt: t0.add(const Duration(seconds: 45)))];
      final work = foldTranscript(entries, density: ChatDensity.normal, turns: turns)[1] as WorkItem;
      expect(work.duration, const Duration(seconds: 45));
      expect(work.turn, 7);
      expect((foldTranscript(entries, density: ChatDensity.normal)[1] as WorkItem).duration, const Duration(seconds: 1), reason: 'with no span, the entries own');
    });

    test('a failed call stays whole, and keeps its turn from folding', () {
      final entries = [
        user('go', 0),
        toolEntry(tool('a', kind: 'read'), 1),
        toolEntry(tool('b', kind: 'execute', status: 'failed'), 2),
        toolEntry(tool('c', kind: 'read'), 3),
        agent('done', 4),
      ];
      expect(shape(foldTranscript(entries, density: ChatDensity.normal)), 'user burst1 tool burst1 agent');
    });

    test('a call that waits or runs stays whole', () {
      final entries = [user('go', 0), toolEntry(tool('a', status: 'pending'), 1), toolEntry(tool('b', status: 'in_progress'), 2)];
      expect(shape(foldTranscript(entries, density: ChatDensity.thinking, working: true)), 'user tool tool');
    });

    test('each turn folds by itself', () {
      final entries = [...worked(), user('again', 100), toolEntry(tool('t3', kind: 'read'), 101), agent('ok', 102)];
      expect(shape(foldTranscript(entries, density: ChatDensity.normal)), 'user work[thought burst2] agent user work[burst1] agent');
    });

    test('an entry of a kind the package does not fold passes through', () {
      final entries = [agent('hello', 0), ChatEntry(MessageRole.notice, 'a notice', at: t0)];
      expect(shape(foldTranscript(entries, density: ChatDensity.normal)), 'agent notice');
    });
  });

  group('summarizeBurst', () {
    test('names each kind of work once, in the order it came, with the files and the lines', () {
      final summary = summarizeBurst([
        tool('t1', kind: 'edit', content: [diff('src/calc.js', 'a - b\n', 'a + b\n')]),
        tool('t2', kind: 'execute'),
      ]);
      expect(summary.text, 'Edited calc.js, ran a command');
      expect((summary.added, summary.removed), (1, 1));
    });

    test('counts commands and names a few files, then only how many', () {
      expect(summarizeBurst([for (var i = 0; i < 4; i++) tool('e$i', kind: 'execute')]).text, 'Ran 4 commands');
      expect(
        summarizeBurst([
          tool('a', kind: 'read', locations: [{'path': '/home/ana/a.dart'}]),
          tool('b', kind: 'read', locations: [{'path': '/home/ana/b.dart', 'line': 3}]),
        ]).text,
        'Read a.dart, b.dart',
      );
      expect(
        summarizeBurst([
          for (final name in ['a', 'b', 'c']) tool(name, kind: 'read', locations: [{'path': '/home/ana/$name.dart'}]),
        ]).text,
        'Read 3 files',
      );
    });

    test('lines of a new file are counted without its final line break, and an unknown kind is a tool', () {
      final summary = summarizeBurst([
        tool('p', kind: 'edit', content: [{'type': 'diff', 'path': 'x.txt', 'newText': 'new\n'}]),
        tool('q', kind: 'think'),
      ]);
      expect(summary.text, 'Edited x.txt, used a tool');
      expect(summary.added, 1);
    });

    test('says how many were stopped', () {
      expect(summarizeBurst([tool('a', kind: 'read', status: 'cancelled'), tool('b', kind: 'read')]).text, 'Read 2 files (1 stopped)');
    });
  });

  test('spanLabel reads as a person would', () {
    expect(spanLabel(const Duration(milliseconds: 300)), 'under a second');
    expect(spanLabel(const Duration(seconds: 42)), '42s');
    expect(spanLabel(const Duration(seconds: 89)), '1m 29s');
    expect(spanLabel(const Duration(minutes: 65)), '1h 5m');
  });
}
