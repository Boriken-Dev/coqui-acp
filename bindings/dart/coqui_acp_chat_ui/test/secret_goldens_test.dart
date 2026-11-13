// The secret cards drawn with the test font (every glyph a square), so the pictures show layout and colour, not words. Regenerate with
// `flutter test --update-goldens test/secret_goldens_test.dart` and look at them before committing.
import 'package:coqui_acp/secret.dart';
import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fixtures.dart';
import 'secret_fixtures.dart';

Widget _scene(Widget child, {Brightness brightness = Brightness.dark}) => MaterialApp(
  debugShowCheckedModeBanner: false,
  theme: theme(brightness: brightness),
  home: Scaffold(body: Center(child: SizedBox(width: 760, height: 1000, child: SingleChildScrollView(child: child)))),
);

void _size(WidgetTester tester) {
  tester.view.physicalSize = const Size(760, 1000);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
}

const _showSchema = '{"type":"object","title":"New API key","properties":{"label":{"type":"string","title":"Label"},"key":{"type":"string","title":"API key","writeOnly":true}}}';

void main() {
  for (final brightness in Brightness.values) {
    testWidgets('a secret request, filled in, ${brightness.name}', (tester) async {
      _size(tester);
      final (params, _) = await tester.runAsync(asked) as (AcpSecretRequestParams, AcpSecretReceiver);
      await tester.pumpWidget(_scene(SecretRequestCard(request: params, environment: environment(), agent: 'Claude', onReply: (_) {}), brightness: brightness));
      await settle(tester);
      await type(tester, 'question-user', 'ana');
      await type(tester, 'secret-password', password);
      await expectLater(find.byType(MaterialApp), matchesGoldenFile('goldens/secret_request_${brightness.name}.png'));
    });
  }

  testWidgets('a secret show, masked and then revealed', (tester) async {
    _size(tester);
    final open = AcpSecretShowOpen(context: AcpSecretContext(sessionId: 's1', requestId: 'w1', message: 'Your new API key', schema: _showSchema, values: '{"label":"ci"}'));
    final deliver = ValueNotifier<AcpSecretShowDeliver?>(null);
    final readied = <AcpSecretShowOpened>[];
    await tester.pumpWidget(_scene(SecretShowCard(open: open, environment: environment(), deliver: deliver, onReady: readied.add, onDone: (_) {}, agent: 'Claude')));
    await settle(tester);
    await expectLater(find.byType(MaterialApp), matchesGoldenFile('goldens/secret_show_asking.png'));
    await tester.tap(find.text('Show'));
    await settle(tester);
    final collector = await tester.runAsync(() => hostChecks().collector(frontend.id, AcpSecretMethod.show, open.context, readied.single.recipient!, now)) as AcpSecretCollector;
    final sealed = {'key': await tester.runAsync(() => collector.seal('key', token)) as AcpSealed};
    deliver.value = AcpSecretShowDeliver(sessionId: 's1', requestId: 'w1', sealed: sealed, auth: await tester.runAsync(() => collector.authenticate(hostProver, open.context.values!, sealed, now)) as AcpSecretBinding);
    await settle(tester);
    await expectLater(find.byType(MaterialApp), matchesGoldenFile('goldens/secret_show_masked.png'));
    await tester.tap(find.byKey(const ValueKey('reveal-key')));
    await tester.pump();
    await expectLater(find.byType(MaterialApp), matchesGoldenFile('goldens/secret_show_revealed.png'));
  });
}
