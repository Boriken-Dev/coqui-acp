// The package is the chat's state and nothing of how it is drawn or carried: it imports no Flutter and no library of the VM or a
// browser alone, so it runs in both and under any UI.
@TestOn('vm')
library;

import 'dart:io';

import 'package:test/test.dart';

void main() {
  final imports = RegExp(
    r'''^\s*(?:import|export|part)\s+['"]([^'"]+)['"]''',
    multiLine: true,
  );

  test(
    'lib imports only the language\'s portable libraries, coqui_acp\'s protocol types and itself',
    () {
      final files = Directory('lib')
          .listSync(recursive: true)
          .whereType<File>()
          .where((f) => f.path.endsWith('.dart'))
          .toList();
      expect(files, isNotEmpty);
      for (final file in files) {
        for (final match in imports.allMatches(file.readAsStringSync())) {
          final target = match.group(1)!;
          final allowed =
              target == 'dart:async' ||
              target == 'dart:collection' ||
              target == 'dart:convert' ||
              target == 'package:coqui_acp/coqui_acp.dart' ||
              target == 'package:coqui_acp_chat/coqui_acp_chat.dart' ||
              (!target.contains(':') && target.endsWith('.dart'));
          expect(allowed, isTrue, reason: '${file.path} imports $target');
        }
      }
    },
  );

  test('the package depends on no Flutter package', () {
    final pubspec = File('pubspec.yaml').readAsStringSync();
    expect(pubspec, isNot(contains('flutter')));
    expect(
      RegExp(
        r'^dependencies:\n((?:  .*\n)+)',
        multiLine: true,
      ).firstMatch(pubspec)!.group(1),
      contains('coqui_acp'),
    );
  });

  test('the protocol library it builds on imports none of them either', () {
    final source = File('../coqui_acp/lib/coqui_acp.dart').readAsStringSync();
    for (final match in imports.allMatches(source)) {
      expect(
        match.group(1),
        anyOf('dart:async', 'dart:convert', 'dart:math', 'src/elicit.dart'),
      );
    }
  });
}
