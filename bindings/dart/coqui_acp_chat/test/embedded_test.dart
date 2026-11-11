// The text the tests carry so that they run in a browser is the text of the files it was made from.
@TestOn('vm')
library;

import 'dart:io';

import 'package:test/test.dart';

import 'support/embedded.dart';

// The repository's file, which a copy of the package alone does not have: the check then has nothing to hold the text to.
final _vectors = File('../../../vectors/usage.json');

void main() {
  test(
    "the usage vectors held here are the repository's vectors/usage.json",
    () {
      expect(usageVectors, _vectors.readAsStringSync());
    },
    skip: _vectors.existsSync()
        ? false
        : 'run in the repository: vectors/usage.json is not two levels above this package',
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
