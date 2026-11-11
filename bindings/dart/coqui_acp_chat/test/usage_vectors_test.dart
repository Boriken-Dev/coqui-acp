// The usage model held to coqui-acp's vectors (`vectors/usage.json`), as the Rust model is: a report written in its one canonical
// form is read and written back unchanged; what a newer writer sent reads as `other` and the report is still read; the fraction and
// the time to a reset are the same integers.
@TestOn('vm')
library;

import 'dart:convert';
import 'dart:io';

import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

void main() {
  final vectors =
      jsonDecode(File('../../../vectors/usage.json').readAsStringSync()) as Map;
  List<Map<dynamic, dynamic>> section(String name) =>
      (vectors[name] as List).cast<Map<dynamic, dynamic>>();

  test('the vectors are all there', () {
    expect(section('roundtrip').length, greaterThanOrEqualTo(7));
    expect(section('lenient').length, greaterThanOrEqualTo(3));
    expect(section('fraction').length, greaterThanOrEqualTo(10));
    expect(section('resets_in').length, greaterThanOrEqualTo(5));
  });

  for (final vector in section('roundtrip')) {
    test('roundtrip: ${vector['name']}', () {
      final read = UsageReport.parse(vector['report']);
      expect(read, isNotNull);
      expect(read!.toJson(), vector['report']);
      // What was written reads the same again.
      expect(
        UsageReport.parse(jsonDecode(jsonEncode(read.toJson())))!.toJson(),
        vector['report'],
      );
    });
  }

  for (final vector in section('lenient')) {
    test('lenient: ${vector['name']}', () {
      final read = UsageReport.parse(vector['read']);
      expect(read, isNotNull);
      expect(read!.toJson(), vector['reads_as']);
      final unknown = vector['unknown'] as Map;
      final bucket = read.buckets.firstOrNull;
      for (final key in unknown.keys) {
        switch (key) {
          case 'unit':
            expect(bucket!.unit.isOther, isTrue);
          case 'status':
            expect(const {
              'ok',
              'warning',
              'exhausted',
              'unknown',
            }, isNot(contains(bucket!.status)));
          case 'source':
            expect(const {
              'reported',
              'inferred',
            }, isNot(contains(bucket!.source)));
          case 'refill':
            expect(bucket!.refill.kind, 'other');
          case 'models':
            expect(bucket!.models.kind, 'other');
          default:
            fail('the vector names $key, which this test does not know');
        }
      }
      // A bucket the agent reports as another kind is still listed and still ranked.
      if (bucket != null) expect(pillText(read, 0), contains(bucket.name));
    });
  }

  for (final vector in section('fraction')) {
    test('fraction: ${vector['name']}', () {
      final given = vector['bucket'] as Map;
      final bucket = UsageBucket(
        id: 'b',
        used: given['used'] as int?,
        limit: given['limit'] as int?,
        remaining: given['remaining'] as int?,
      );
      expect(bucket.fractionPermille, vector['permille']);
    });
  }

  for (final vector in section('resets_in')) {
    test('resets_in: ${vector['name']}', () {
      final report = UsageReport.parse({
        'supported': true,
        'revision': 0,
        'observed_at': 0,
        'buckets': [
          {
            'id': 'b',
            'models': {'kind': 'all'},
            'unit': 'tokens',
            'refill': vector['refill'],
            'status': 'ok',
            'observed_at': 0,
            'source': 'reported',
          },
        ],
      })!;
      expect(
        report.buckets.single.resetsIn(vector['now'] as int),
        vector['seconds'],
      );
    });
  }

  test('what is not a report is not read', () {
    for (final json in <Object?>[
      null,
      'x',
      3,
      <Object?>[],
      <String, Object?>{},
      {'supported': 'yes'},
    ]) {
      expect(UsageReport.parse(json), isNull, reason: '$json');
    }
  });
}
