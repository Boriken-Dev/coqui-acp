// What a conversation keeps of the usage an agent reports: the newest report by revision, the spend that is its own, a read after
// each turn, and the words a usage control shows it in.
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
        expect(spendText(session.spend), r'1.5k tokens · $0.05');
        store.dispose();
      },
    );

    test('an absent cost stays absent', () async {
      final agent = FakeConnection(usage: true)
        ..report = _report(100, spend: _spend('s1'));
      final (:store, :session, agent: _) = started(connection: agent);
      await settle();
      expect(spendText(session.spend), '1.5k tokens');
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
          spendText(session.spend),
          r'1.5k tokens · $2.00',
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

  group('the text', () {
    test('counts are compact', () {
      expect(
        [0, 950, 1000, 1500, 120000, 2400000, 3000000000].map(compactCount),
        ['0', '950', '1k', '1.5k', '120k', '2.4M', '3G'],
      );
    });

    test('money rounds to cents', () {
      expect([0, 4999, 5000, 1200000, 5000000].map(dollars), [
        r'$0.00',
        r'$0.00',
        r'$0.01',
        r'$1.20',
        r'$5.00',
      ]);
    });

    test('a span is its two largest units', () {
      expect([30, 60, 600, 4800, 7200, 90000, 100000].map(spanText), [
        '30 s',
        '1 m',
        '10 m',
        '1 h 20 m',
        '2 h',
        '1 d 1 h',
        '1 d 3 h',
      ]);
    });

    test('a bucket gives its level against the threshold', () {
      UsageBucket at(int used, {String status = 'ok'}) =>
          UsageBucket(id: 'b', used: used, limit: 1000, status: status);
      expect(levelOf(at(799), 0), UsageLevel.calm);
      expect(levelOf(at(800), 0), UsageLevel.warn);
      expect(levelOf(at(799), 0, warnPermille: 500), UsageLevel.warn);
      expect(
        levelOf(at(100, status: 'warning'), 0),
        UsageLevel.warn,
        reason: 'the agent said so',
      );
      expect(levelOf(at(1000, status: 'exhausted'), 0), UsageLevel.danger);
    });

    test(
      'the bucket that matters most is shown, an exhausted one before the rest',
      () {
        final report = UsageReport(
          supported: true,
          buckets: [
            const UsageBucket(id: 'a', label: 'Day', used: 900, limit: 1000),
            const UsageBucket(
              id: 'b',
              label: 'Week',
              used: 100,
              limit: 1000,
              status: 'exhausted',
            ),
          ],
        );
        expect(pillText(report, 0), 'Week · 10%');
        expect(pillText(const UsageReport(supported: true), 0), 'No usage yet');
      },
    );
  });
}
