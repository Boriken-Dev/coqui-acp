import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'changes_test.dart' show edits;
import 'fixtures.dart';

/// Records each call and marks its line, so a test sees which lines went through the hook and with what.
class _Marker {
  final calls = <(String, String?)>[];
  static const mark = Color(0xFF00AA00);

  TextSpan call(String line, String? path, TextStyle style) {
    calls.add((line, path));
    return TextSpan(text: line, style: const TextStyle(color: mark));
  }
}

ChatLook withSpans(CodeSpans? spans) {
  final base = theme().extension<ChatLook>()!;
  return base.copyWith(codeSpans: spans);
}

/// The colours of the text spans under [of].
Iterable<Color?> colours(WidgetTester tester, Finder of) => tester
    .widgetList<RichText>(find.descendant(of: of, matching: find.byType(RichText)))
    .expand((rich) => _leaves(rich.text))
    .map((span) => span.style?.color);

Iterable<TextSpan> _leaves(InlineSpan span) sync* {
  if (span is TextSpan) {
    if (span.text != null) yield span;
    for (final child in span.children ?? const <InlineSpan>[]) {
      yield* _leaves(child);
    }
  }
}

void main() {
  const options = [AcpPermissionOption(optionId: 'a', name: 'Allow', kind: 'allow_once')];
  const change = ToolDiff(path: 'src/calc.js', oldText: 'return a - b;', newText: 'return a + b;');

  testWidgets('with no hook, a changed line is one plain Text and the look has none', (tester) async {
    await tester.pumpWidget(host(DiffView(lines: withContext(diffLines('a\nb', 'a\nc'))), look: withSpans(null)));
    expect(find.text('- b'), findsOneWidget);
    expect(find.text('+ c'), findsOneWidget);
    expect(withSpans(null).codeSpans, isNull);
  });

  testWidgets('DiffView draws every line through the hook with the path it was given, the marker staying plain', (tester) async {
    final marker = _Marker();
    await tester.pumpWidget(host(DiffView(lines: withContext(diffLines('a\nb', 'a\nc')), path: 'x.rs'), look: withSpans(marker.call)));
    expect(marker.calls, [('a', 'x.rs'), ('b', 'x.rs'), ('c', 'x.rs')]);
    expect(find.text('- b'), findsOneWidget, reason: 'plain text of the whole line is unchanged');
    expect(colours(tester, find.byType(DiffView)), contains(_Marker.mark));
  });

  testWidgets('ChangesPane draws each shown line through the hook with its file path', (tester) async {
    final marker = _Marker();
    await tester.pumpWidget(host(
      ChangesPane(files: changesOf(edits()), isViewed: (_) => false, onViewed: (_, _) {}, onComment: (_) {}, onClose: () {}),
      look: withSpans(marker.call),
    ));
    expect(marker.calls.where((call) => call.$2 == 'src/calc.js').map((call) => call.$1).toList(), containsAll(['one', 'two', '2', 'three']));
    expect(marker.calls.where((call) => call.$2 == 'src/new.js').map((call) => call.$1), containsAll(['fresh']));
    expect(colours(tester, find.byKey(const ValueKey('line src/new.js new 1'))), contains(_Marker.mark));
    expect(find.textContaining('+ fresh'), findsOneWidget);
  });

  testWidgets('a permission card\'s diff draws through the hook with the edit\'s path', (tester) async {
    final marker = _Marker();
    await tester.pumpWidget(host(
      PermissionCard(agent: 'Claude', title: 'Edit calc.js', diff: change, options: options, onChoose: (_) {}),
      look: withSpans(marker.call),
    ));
    expect(marker.calls, [('return a - b;', 'src/calc.js'), ('return a + b;', 'src/calc.js')]);
  });

  testWidgets('a tool call\'s diff and patch go through the hook, a patch of one file with its path', (tester) async {
    final marker = _Marker();
    final call = tool('t', kind: 'edit', content: [diff('f.py', 'x', 'y')]);
    await tester.pumpWidget(host(ToolBlockView(tool: call, block: change), look: withSpans(marker.call)));
    expect(marker.calls.map((c) => c.$2).toSet(), {'src/calc.js'});
    marker.calls.clear();
    await tester.pumpWidget(host(
      ToolBlockView(tool: call, block: const ToolPatch('@@ -1 +1 @@\n-a\n+b', paths: ['p.dart'])),
      look: withSpans(marker.call),
    ));
    expect(marker.calls, [('a', 'p.dart'), ('b', 'p.dart')]);
    marker.calls.clear();
    await tester.pumpWidget(host(
      ToolBlockView(tool: call, block: const ToolPatch('@@ -1 +1 @@\n-a\n+b', paths: ['p.dart', 'q.dart'])),
      look: withSpans(marker.call),
    ));
    expect(marker.calls.map((c) => c.$2).toSet(), {null});
  });

  test('copyWith keeps the hook and lerp keeps the nearer one', () {
    final marker = _Marker();
    final look = withSpans(marker.call);
    expect(look.copyWith(radius: 3).codeSpans, same(look.codeSpans));
    expect(look.lerp(withSpans(null), 0.2).codeSpans, isNotNull);
    expect(look.lerp(withSpans(null), 0.8).codeSpans, isNull);
  });
}
