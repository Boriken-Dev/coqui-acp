// What a conversation keeps of the usage an agent reports: the newest report by revision, the spend that is its own, a read after
// each turn, and the bounds on what the agent sends.
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

UsageReport _report(
  int used, {
  int revision = 1,
  UsageSpend? spend,
  bool supported = true,
}) => UsageReport(
  supported: supported,
  revision: revision,
  buckets: [
    UsageBucket(id: 'five_hour', label: '5 hours', used: used, limit: 1000),
  ],
  spend: spend,
);

UsageSpend _spend(String session, {int? cost}) => UsageSpend(
  session,
  input: 1200,
  output: 340,
  cacheRead: 9000,
  cacheWrite: 800,
  costMicroUsd: cost,
);

void main() {
  group('what the conversation holds', () {
    test('nothing, and no read, for an agent that reports no usage', () async {
      final (:store, :session, :agent) = started();
      expect(session.reportsUsage, isFalse);
      expect(agent.fetches, 0);
      store.dispose();
    });

    test(
      'the report the agent gives when asked, without its spend, which is kept apart',
      () async {
        final agent = FakeConnection(usage: true)
          ..report = _report(100, spend: _spend('s1', cost: 48500));
        final (:store, :session, agent: _) = started(connection: agent);
        await settle();
        expect(session.reportsUsage, isTrue);
        expect(session.usageReport!.buckets.single.used, 100);
        expect(session.usageReport!.spend, isNull);
        expect(session.spend!.input + session.spend!.output, 1540);
        expect(session.spend!.costMicroUsd, 48500);
        store.dispose();
      },
    );

    test('an absent cost stays absent', () async {
      final agent = FakeConnection(usage: true)
        ..report = _report(100, spend: _spend('s1'));
      final (:store, :session, agent: _) = started(connection: agent);
      await settle();
      expect(session.spend!.costMicroUsd, isNull);
      store.dispose();
    });

    test(
      'the newest report by revision, whatever order they arrive in, and a spend for this conversation only',
      () async {
        final agent = FakeConnection(usage: true)..report = _report(100);
        final (:store, :session, agent: _) = started(connection: agent);
        await settle();
        agent.notice(_report(400, revision: 5));
        await settle();
        expect(session.usageReport!.buckets.single.used, 400);
        agent.notice(
          _report(200, revision: 4, spend: _spend('s1', cost: 2000000)),
        );
        await settle();
        expect(
          session.usageReport!.buckets.single.used,
          400,
          reason: 'the older report does not replace it',
        );
        expect(
          session.spend!.costMicroUsd,
          2000000,
          reason: 'but a spend is kept whatever its revision',
        );
        agent.notice(
          _report(500, revision: 6, spend: _spend('other')),
          session: 'other',
        );
        await settle();
        expect(
          session.spend!.sessionId,
          's1',
          reason: 'another conversation\'s spend is not this one\'s',
        );
        expect(
          session.usageReport!.buckets.single.used,
          500,
          reason: 'the account\'s allowances are for every conversation',
        );
        store.dispose();
      },
    );

    test(
      'a spend is this conversation\'s when the notice names it, whichever session the spend names',
      () async {
        final agent = FakeConnection(usage: true)..report = _report(100);
        final (:store, :session, agent: _) = started(connection: agent);
        await settle();
        agent.notice(
          _report(100, revision: 2, spend: _spend('o1')),
          session: 's1',
        );
        await settle();
        expect(session.spend!.sessionId, 'o1');
        store.dispose();
      },
    );

    test(
      'an agent that says it is not supported shows none from then on',
      () async {
        final agent = FakeConnection(usage: true)..report = _report(100);
        final (:store, :session, agent: _) = started(connection: agent);
        await settle();
        agent.notice(const UsageReport(supported: false, revision: 9));
        await settle();
        expect(session.reportsUsage, isFalse);
        expect(session.usageReport, isNull);
        store.dispose();
      },
    );

    test('is read again when a turn ends', () async {
      final agent = FakeConnection(usage: true)..report = _report(100);
      final (:store, :session, agent: _) = started(connection: agent);
      await settle();
      final asked = agent.fetches;
      feed(session, state('running'));
      feed(session, state('idle', reason: 'end_turn'));
      await settle();
      expect(agent.fetches, greaterThan(asked));
      store.dispose();
    });
  });

  group('what an agent sends', () {
    test('is read as lines a person can be shown, and cut at the limits', () {
      final cr = String.fromCharCode(0x202e);
      final report = UsageReport.parse({
        'supported': true,
        'sources': ['reported', 5, 'a', 'b', 'c'],
        'account': 'acct\n$cr${'x' * 40}',
        'buckets': [
          for (var i = 0; i < 6; i++)
            {
              'id': 'b$i',
              'label': 'Label\u0007$i ${'y' * 40}',
              'models': {
                'kind': 'only',
                'models': ['m1', 'm2', 'm3', 'm4'],
              },
              'unit': 'tokens',
              'status': 'ok',
              'source': 'reported',
            },
          {'id': '   ', 'unit': 'tokens'},
        ],
        'spend': {
          'session_id': 's',
          'by_model': [
            for (var i = 0; i < 5; i++) {'model': 'model$i'},
          ],
        },
      }, const ChatLimits(shortText: 12, usageBuckets: 3))!;
      expect(report.sources, [
        'reported',
        'a',
      ], reason: 'three looked at, one of them no text');
      expect(report.account, 'acct  ${'x' * 6}…');
      expect(report.buckets.map((b) => b.id), ['b0', 'b1', 'b2']);
      expect(report.buckets.first.label, 'Label 0 yyyy…');
      expect(report.buckets.first.models.models, ['m1', 'm2', 'm3']);
      expect(report.spend!.byModel, hasLength(3));
    });

    test('a bucket without a usable id is not read', () {
      final report = UsageReport.parse({
        'supported': true,
        'buckets': [
          {'id': '\n'},
          {'id': 7},
        ],
      })!;
      expect(report.buckets, isEmpty);
    });
  });
}
