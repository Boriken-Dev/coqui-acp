// What the secret-card tests and goldens share: two identities, the host's side of a request and of a show, and the screen.
import 'package:coqui_acp/secret.dart';
import 'package:coqui_acp/secret_unprotected.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';

const now = 1700000000;
final frontendKey = List<int>.generate(32, (i) => i + 1);
final hostKey = List<int>.generate(32, (i) => i + 101);
final strangerKey = List<int>.generate(32, (i) => i + 201);

final frontend = ecdsaP256Prover(frontendKey);
final hostProver = ecdsaP256Prover(hostKey);
final stranger = ecdsaP256Prover(strangerKey);

/// What the frontend knows: its own id, and the host's.
AcpSecretVerifier frontendChecks() => AcpSecretVerifier(own: frontend.id, known: [hostProver.id, stranger.id]);

/// What the host knows: its own id, and the frontend's.
AcpSecretVerifier hostChecks() => AcpSecretVerifier(own: hostProver.id, known: [frontend.id]);

SecretCardEnvironment environment() => SecretCardEnvironment(prover: frontend, verifier: frontendChecks(), expected: hostProver.id, host: 'Canoa', now: () => now);

const password = 'hunter2-correct-horse';
const token = 'tok-9f8e7d6c5b4a';

const schema = '{"type":"object","title":"Registry login","properties":{'
    '"user":{"type":"string","title":"User name","description":"Your account on the registry"},'
    '"password":{"type":"string","title":"Password","writeOnly":true,"minLength":4},'
    '"pin":{"type":"integer","title":"PIN","writeOnly":true,"minimum":0,"maximum":99999},'
    '"region":{"type":"string","title":"Region","enum":["eu","us"]},'
    '"remember":{"type":"boolean","title":"Remember for this session"}},'
    '"required":["user","password"]}';

AcpSecretContext requestContext({String text = schema, String id = 'r1', String message = 'The deploy needs your registry login'}) =>
    AcpSecretContext(sessionId: 's1', requestId: id, message: message, schema: text);

/// What the host sends: a receiver of its own key pair, bound to the host's identity for the frontend.
Future<(AcpSecretRequestParams, AcpSecretReceiver)> asked({String text = schema, AcpSecretProver? as, String id = 'r1'}) async {
  final context = requestContext(text: text, id: id);
  final receiver = await AcpSecretReceiver.create(AcpSecretMethod.request, context, as ?? hostProver, frontend.id, now);
  return (AcpSecretRequestParams(context: context, recipient: receiver.recipient), receiver);
}

Future<void> settle(WidgetTester tester) async {
  await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 80)));
  await tester.pump();
}

Future<void> type(WidgetTester tester, String key, String text) async {
  await tester.enterText(find.byKey(ValueKey(key)), text);
  await tester.pump();
}

Finder button(String label) => find.widgetWithText(FilledButton, label);

/// What went to the clipboard, which no card of a request may touch.
final clipboard = <String>[];

void watchClipboard() {
  clipboard.clear();
  TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.platform, (call) async {
    if (call.method == 'Clipboard.setData') clipboard.add((call.arguments as Map)['text'] as String);
    return null;
  });
  addTearDown(() => TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.platform, null));
}

void bigScreen(WidgetTester tester) {
  tester.view.physicalSize = const Size(1000, 1800);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
}
