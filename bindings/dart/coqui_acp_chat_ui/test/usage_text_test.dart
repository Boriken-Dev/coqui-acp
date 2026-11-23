// How a bucket's models and a window that does not apply are worded.

import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('the models of a bucket are the names the harness gave, else its ids, else every model', () {
    const named = UsageBucket(
      id: 'a',
      models: UsageCoverage('only', ['claude-opus-5-5-high']),
      modelNames: ['Claude Opus 5.5', 'GPT-OSS 120B'],
    );
    expect(modelsOf(named), 'Claude Opus 5.5, GPT-OSS 120B');
    expect(modelsOf(const UsageBucket(id: 'b', models: UsageCoverage('only', ['opus']))), 'opus');
    expect(modelsOf(const UsageBucket(id: 'c', models: UsageCoverage.all)), 'every model');
    expect(modelsOf(const UsageBucket(id: 'd')), 'models not known');
    expect(bucketDetails(named, 0), contains('covers Claude Opus 5.5, GPT-OSS 120B'));
  });

  test('a window that does not apply says so, and is listed after the ones that do', () {
    const off = UsageBucket(id: 'off', label: 'Five Hour Limit Remaining', status: 'disabled');
    const used = UsageBucket(id: 'on', label: 'Weekly Limit Remaining', unit: UsageUnit.permille, limit: 1000, remaining: 100);
    expect(bucketLine(off, 0), 'Five Hour Limit Remaining · disabled');
    final report = const UsageReport(supported: true, buckets: [off, used]);
    expect([for (final bucket in ordered(report, 0)) bucket.id], ['on', 'off']);
    expect(levelOf(off, 0), UsageLevel.calm);
  });
}
