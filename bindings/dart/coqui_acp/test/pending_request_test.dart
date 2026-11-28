// A request whose answer nobody awaits yet when the link ends during its send.
@TestOn('vm')
library;

import 'dart:async';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:test/test.dart';

void main() {
  pendingRequestEndedDuringSend();
}

void pendingRequestEndedDuringSend() {
  test(
    'a link that ends while a request is being sent is no unhandled error',
    () async {
      final unhandled = <Object>[];
      final toClient = StreamController<String>();
      final sending = Completer<void>();
      late Future<Object?> outcome;
      await runZonedGuarded(() async {
        final client = AcpClient(
          incoming: toClient.stream,
          send: (_) async {
            // The link ends before the send settles.
            toClient.addError(StateError('link ended'));
            await Future<void>.delayed(const Duration(milliseconds: 50));
            sending.complete();
          },
        );
        // The caller awaits only after the link has ended.
        outcome = client.initialize().then<Object?>(
          (_) => null,
          onError: (Object e) => e,
        );
        await sending.future;
        await Future<void>.delayed(const Duration(milliseconds: 50));
      }, (error, _) => unhandled.add(error));
      expect(unhandled, isEmpty);
      expect(await outcome, isA<StateError>());
    },
  );
}
