// The first message of a conversation whose agent has not started: shown at once as the person's own, replaced by the message
// that is sent, and taken out again when nothing is.
import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

List<String> _texts(ChatSession session) => [for (final m in session.messages) m.text];

void main() {
  test('a held first message shows with what goes with it, and is replaced by the one sent, once', () async {
    final (:store, :session, :agent) = started();
    session.holdFirst('Fix the build', attachments: ['notes.txt']);
    expect(_texts(session), ['Fix the build', 'Sent with this: notes.txt']);
    expect(session.heldFirst?.role, MessageRole.user);
    final shown = session.heldFirst!.at;

    await session.send('Fix the build', attachments: [AcpAttachment.text('notes.txt', 'hi')]);
    expect(_texts(session), ['Fix the build', 'Sent with this: notes.txt']);
    expect(session.heldFirst, isNull);
    expect(session.messages.first.at, shown, reason: 'the message keeps the time it was shown at');
    store.dispose();
  });

  test('a held first message that is released leaves nothing behind', () async {
    final (:store, :session, :agent) = started();
    session.holdFirst('Fix the build', attachments: ['notes.txt']);
    session.releaseFirst();
    expect(session.messages, isEmpty);
    expect(session.heldFirst, isNull);
    store.dispose();
  });
}
