import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fixtures.dart';

List<ChatEntry> edits() => [
  user('go', 0),
  toolEntry(tool('a', kind: 'edit', content: [diff('src/calc.js', 'one\ntwo\nthree\n', 'one\n2\nthree\n')]), 1),
  toolEntry(
    tool('b', kind: 'edit', content: [{'type': 'diff', 'path': 'src/new.js', 'newText': 'fresh\n'}]),
    2,
  ),
  toolEntry(tool('c', kind: 'edit', status: 'failed', content: [diff('src/never.js', 'a', 'b')]), 3),
];

void main() {
  group('changesOf', () {
    test('lists the files completed tools changed, in the order first changed, and skips a failed edit', () {
      final files = changesOf(edits());
      expect(files.map((file) => file.path), ['src/calc.js', 'src/new.js']);
      expect((files[0].added, files[0].removed), (1, 1));
      expect(files[1].before, isNull);
    });

    test('keeps the first before and the last after of a file changed twice', () {
      final entries = [
        toolEntry(tool('a', content: [diff('f', 'v1', 'v2')]), 1),
        toolEntry(tool('b', content: [diff('f', 'v2', 'v3')]), 2),
      ];
      final file = changesOf(entries).single;
      expect((file.before, file.after), ('v1', 'v3'));
    });

    test('numbers the lines of a change by their place in the old and the new text', () {
      final entries = [
        toolEntry(tool('p', content: [{'type': 'diff', 'path': 'p.txt', 'oldText': 'a\nb\n', 'newText': 'a\nc\n'}]), 2),
      ];
      final lines = changesOf(entries).single.lines.where((line) => line.kind != DiffKind.same).toList();
      expect(lines.map((line) => (line.kind, line.old, line.now)), [(DiffKind.removed, 2, null), (DiffKind.added, null, 2)]);
    });

    test('a changed file has a new version, so a mark of viewed holds only for what was viewed', () {
      final first = changesOf([toolEntry(tool('a', content: [diff('f', 'v1', 'v2')]), 1)]).single;
      final again = changesOf([toolEntry(tool('a', content: [diff('f', 'v1', 'v2')]), 1)]).single;
      final later = changesOf([toolEntry(tool('a', content: [diff('f', 'v1', 'v3')]), 1)]).single;
      expect(first.version, again.version);
      expect(first.version, isNot(later.version));
    });
  });

  group('ChangesPane', () {
    Widget pane({Set<String> viewed = const {}, List<String>? log, ValueChanged<String>? onComment}) => host(ChangesPane(
      files: changesOf(edits()),
      isViewed: (file) => viewed.contains(file.path),
      onViewed: (file, on) => log?.add('${file.path} $on'),
      onComment: onComment ?? (_) {},
      onClose: () => log?.add('close'),
    ));

    testWidgets('counts what was viewed, and a viewed file starts folded', (tester) async {
      await tester.pumpWidget(pane(viewed: {'src/calc.js'}));
      expect(find.text('1 of 2 viewed'), findsOneWidget);
      expect(find.byKey(const ValueKey('line src/calc.js new 2')), findsNothing);
      expect(find.byKey(const ValueKey('line src/new.js new 1')), findsOneWidget);
    });

    testWidgets('the checkbox marks a file viewed and folds it', (tester) async {
      final log = <String>[];
      await tester.pumpWidget(pane(log: log));
      await tester.tap(find.byKey(const ValueKey('viewed src/calc.js')));
      await tester.pump();
      expect(log, ['src/calc.js true']);
      expect(find.byKey(const ValueKey('line src/calc.js new 2')), findsNothing);
    });

    testWidgets('choosing a line comments on it, with its file and line', (tester) async {
      String? comment;
      await tester.pumpWidget(pane(onComment: (text) => comment = text));
      await tester.tap(find.byKey(const ValueKey('line src/calc.js new 2')));
      await tester.pumpAndSettle();
      await tester.enterText(find.byKey(const Key('change-comment')), 'use a name');
      await tester.tap(find.text('Add to message'));
      await tester.pumpAndSettle();
      expect(comment, 'src/calc.js:2: `2`\nuse a name\n');
    });

    testWidgets('a removed line is named by its old number, and an empty comment adds nothing', (tester) async {
      String? comment;
      await tester.pumpWidget(pane(onComment: (text) => comment = text));
      await tester.tap(find.byKey(const ValueKey('line src/calc.js old 2')));
      await tester.pumpAndSettle();
      expect(find.text('Comment on src/calc.js, removed line 2'), findsOneWidget);
      await tester.tap(find.text('Add to message'));
      await tester.pumpAndSettle();
      expect(comment, isNull);
    });

    testWidgets('with no change it says so, and the close button closes', (tester) async {
      final log = <String>[];
      await tester.pumpWidget(host(ChangesPane(files: const [], isViewed: (_) => false, onViewed: (_, _) {}, onComment: (_) {}, onClose: () => log.add('close'))));
      expect(find.text('No file has changed yet.'), findsOneWidget);
      await tester.tap(find.byTooltip('Close changes'));
      expect(log, ['close']);
    });
  });

  test('DiffLine counting treats a hunk header as the start of a hunk', () {
    final lines = patchLines('--- a\n+++ b\n@@ -1 +1 @@\n-gone\n+arrived\n kept');
    expect(lines[2], isNull);
    expect(lines[3]!.kind, DiffKind.removed);
    expect(lines[4]!.text, 'arrived');
    expect(lines[5]!.kind, DiffKind.same);
  });

  testWidgets('DiffView draws added and removed lines on the look\'s colours', (tester) async {
    await tester.pumpWidget(host(DiffView(lines: withContext(diffLines('a\nb', 'a\nc')))));
    final look = tester.element(find.byType(DiffView)).chatLook;
    final colors = tester.widgetList<Container>(find.descendant(of: find.byType(DiffView), matching: find.byType(Container))).map((box) => box.color).whereType<Color>();
    expect(colors, containsAll([look.added, look.removed]));
  });
}
