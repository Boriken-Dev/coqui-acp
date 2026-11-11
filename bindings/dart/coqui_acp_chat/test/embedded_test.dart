// The text the tests carry so that they run in a browser is the text of the files it was made from.
@TestOn('vm')
library;

import 'dart:io';

import 'package:test/test.dart';

import 'support/embedded.dart';

void main() {
  test(
    "the usage vectors held here are the repository's vectors/usage.json",
    () {
      expect(
        usageVectors,
        File('../../../vectors/usage.json').readAsStringSync(),
      );
    },
  );

  test(
    'the recorded updates are one JSON object a line, with no text of anyone\'s',
    () {
      final lines = codingTaskUpdates
          .split('\n')
          .where((line) => line.trim().isNotEmpty);
      expect(lines, isNotEmpty);
      for (final line in lines) {
        expect(line, startsWith('{"kind":'));
      }
    },
  );
}
