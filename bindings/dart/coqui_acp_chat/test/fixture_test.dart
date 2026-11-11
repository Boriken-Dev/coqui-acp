// Every update a real agent sent in a coding task, recorded as its kind and shape and never its text, plus the v2 kinds that
// run lacked, reaches a rendering of its own in a conversation: a kind that reached the fallback would show nothing.
@TestOn('vm')
library;

import 'dart:convert';
import 'dart:io';

import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

/// A shape made an update again, with a stand-in for each value.
Object? _filled(Object? shape) => switch (shape) {
  'string' => 'x',
  'int' => 1,
  'double' => 0.5,
  'bool' => true,
  'null' => null,
  final Map<dynamic, dynamic> map => {
    for (final entry in map.entries) '${entry.key}': _filled(entry.value),
  },
  final List<dynamic> list => [for (final item in list) _filled(item)],
  _ => shape,
};

void main() {
  final fixture = [
    for (final line in File(
      'test/fixtures/coding_task_updates.jsonl',
    ).readAsLinesSync())
      if (line.trim().isNotEmpty) jsonDecode(line) as Map<String, dynamic>,
  ];

  test('every update it sent has a rendering of its own, and applies', () {
    final recorder = UpdateRecorder();
    final session = ChatSession(id: 'x', sessionId: 's1', recorder: recorder);
    for (final seen in fixture) {
      feed(session, (_filled(seen['shape'])! as Map).cast<String, Object?>());
    }
    expect(recorder.seen.map((seen) => seen.kind).toSet(), {
      for (final seen in fixture) seen['kind'],
    });
    expect(
      recorder.unshown,
      isEmpty,
      reason: 'these kinds reached the fallback, which shows nothing',
    );
    for (final seen in fixture) {
      final kind = recorder.seen.firstWhere((s) => s.kind == seen['kind']);
      expect(
        kind.rendering.name,
        seen['rendering'],
        reason: 'the kind is taken as it was when it was recorded',
      );
    }
  });
}
