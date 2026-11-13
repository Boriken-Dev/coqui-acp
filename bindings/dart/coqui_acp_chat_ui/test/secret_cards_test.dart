// The secret-prompt cards: the form from a request's schema, a show of sealed values, and the two walls between a secret and the rest of
// the app: nothing typed is echoed, copied or kept, and the only thing a request hands out is the sealed answer.
import 'dart:convert';

import 'package:coqui_acp/secret.dart';
import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fixtures.dart';

import 'secret_fixtures.dart';

void main() {
  group('SecretRequestCard', () {
    testWidgets('draws the schema in rows, says who asks and why, and masks the secret fields', (tester) async {
      bigScreen(tester);
      final (params, _) = await tester.runAsync(asked) as (AcpSecretRequestParams, AcpSecretReceiver);
      await tester.pumpWidget(host(SecretRequestCard(request: params, environment: environment(), agent: 'Claude', onReply: (_) {})));
      await settle(tester);
      expect(find.text('Claude asks for a secret'), findsOneWidget);
      expect(find.text('Registry login'), findsOneWidget);
      expect(find.text('The deploy needs your registry login'), findsOneWidget);
      expect(find.textContaining('never sent to the agent'), findsOneWidget);
      expect(find.text('User name *'), findsOneWidget);
      expect(find.text('Your account on the registry'), findsOneWidget);
      expect(find.text('Password *'), findsOneWidget);
      expect(find.text('PIN'), findsOneWidget);
      expect(find.byType(ListTile), findsWidgets);
      for (final name in ['password', 'pin']) {
        final field = tester.widget<TextField>(find.byKey(ValueKey('secret-$name')));
        expect(field.obscureText, isTrue, reason: name);
        expect(field.enableInteractiveSelection, isFalse, reason: name);
        expect(field.enableSuggestions, isFalse);
        expect(field.autocorrect, isFalse);
        expect(field.autofillHints, isNull);
      }
      expect(tester.widget<TextField>(find.byKey(const ValueKey('question-user'))).obscureText, isFalse);
    });

    testWidgets('a secret field has no way to copy out: no menu, no copy shortcut, nothing on the clipboard', (tester) async {
      bigScreen(tester);
      watchClipboard();
      final (params, _) = await tester.runAsync(asked) as (AcpSecretRequestParams, AcpSecretReceiver);
      await tester.pumpWidget(host(SecretRequestCard(request: params, environment: environment(), onReply: (_) {})));
      await settle(tester);
      await type(tester, 'secret-password', password);
      final editable = find.descendant(of: find.byKey(const ValueKey('secret-password')), matching: find.byType(EditableText));
      final field = tester.widget<TextField>(find.byKey(const ValueKey('secret-password')));
      expect(field.contextMenuBuilder!(tester.element(editable), tester.state<EditableTextState>(editable)), isA<SizedBox>());
      await tester.tap(find.byKey(const ValueKey('secret-password')));
      await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
      await tester.sendKeyEvent(LogicalKeyboardKey.keyA);
      await tester.sendKeyEvent(LogicalKeyboardKey.keyC);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
      await tester.pump();
      expect(clipboard, isEmpty);
      expect(tester.widget<EditableText>(editable).obscureText, isTrue);
      expect(tester.widget<EditableText>(editable).enableInteractiveSelection, isFalse);
    });

    testWidgets('Send sealed waits for the required fields and for values that fit', (tester) async {
      bigScreen(tester);
      final (params, _) = await tester.runAsync(asked) as (AcpSecretRequestParams, AcpSecretReceiver);
      await tester.pumpWidget(host(SecretRequestCard(request: params, environment: environment(), onReply: (_) {})));
      await settle(tester);
      expect(tester.widget<FilledButton>(button('Send sealed')).onPressed, isNull);
      expect(find.text('Password is required.'), findsOneWidget);
      await type(tester, 'secret-password', password);
      expect(find.text('User name is required.'), findsOneWidget);
      await type(tester, 'question-user', 'ana');
      await type(tester, 'secret-password', 'abc');
      expect(find.text('Password does not fit what was asked.'), findsOneWidget);
      await type(tester, 'secret-password', password);
      expect(tester.widget<FilledButton>(button('Send sealed')).onPressed, isNotNull);
      await type(tester, 'secret-pin', '123456');
      expect(tester.widget<FilledButton>(button('Send sealed')).onPressed, isNull);
      await type(tester, 'secret-pin', '4321');
      expect(tester.widget<FilledButton>(button('Send sealed')).onPressed, isNotNull);
    });

    testWidgets('Submit seals the secret fields to the host\'s key, and the answer holds none of the typed secrets', (tester) async {
      bigScreen(tester);
      watchClipboard();
      final (params, receiver) = await tester.runAsync(asked) as (AcpSecretRequestParams, AcpSecretReceiver);
      final replies = <AcpSecretRequestReply>[];
      await tester.pumpWidget(host(SecretRequestCard(request: params, environment: environment(), onReply: replies.add)));
      await settle(tester);
      await type(tester, 'question-user', 'ana');
      await type(tester, 'secret-password', password);
      await type(tester, 'secret-pin', '4321');
      await tester.tap(find.byKey(const ValueKey('question-remember')));
      await tester.pump();
      await tester.tap(find.byKey(const ValueKey('question-region')));
      await tester.pumpAndSettle();
      await tester.tap(find.text('us').last);
      await tester.pumpAndSettle();
      await tester.tap(button('Send sealed'));
      await settle(tester);

      expect(replies, hasLength(1));
      final reply = replies.single as AcpSecretAccepted;
      // On the wire: the plain fields as text, the secrets as sealed blobs, the binding.
      final wire = jsonEncode(reply.toJson());
      expect(wire, isNot(contains(password)));
      expect(wire, isNot(contains('4321')));
      expect(jsonDecode(reply.plain), {'user': 'ana', 'region': 'us', 'remember': true});
      expect(reply.sealed.keys.toSet(), {'password', 'pin'});
      // Exactly what the binding seals: the host, who knows the frontend, verifies and opens the same values.
      final opened = await tester.runAsync(() => receiver.openValues(hostChecks(), frontend.id, reply.plain, reply.sealed, reply.auth, now)) as AcpOpened;
      expect(opened.secrets['password']!.text, password);
      expect(opened.secrets['pin']!.text, '4321');
      expect(opened.plain['user'], 'ana');
      expect(clipboard, isEmpty);
      // Nothing typed stays in the form.
      expect(tester.widget<TextField>(find.byKey(const ValueKey('secret-password'))).controller!.text, isEmpty);
      expect(tester.widget<TextField>(find.byKey(const ValueKey('secret-pin'))).controller!.text, isEmpty);
    });

    testWidgets('the values go only to the host\'s channel: the one callback gets the sealed answer, and the card has no other way out', (tester) async {
      bigScreen(tester);
      watchClipboard();
      final (params, receiver) = await tester.runAsync(asked) as (AcpSecretRequestParams, AcpSecretReceiver);
      final out = <Object?>[];
      // Every channel the card could write to: its reply, and the platform's clipboard and text-input channels (watched above).
      final platform = <String>[];
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.platform, (call) async {
        platform.add('${call.method} ${call.arguments}');
        return null;
      });
      addTearDown(() => TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.platform, null));
      await tester.pumpWidget(host(SecretRequestCard(request: params, environment: environment(), onReply: (reply) => out.add(jsonEncode(reply.toJson())))));
      await settle(tester);
      await type(tester, 'question-user', 'ana');
      await type(tester, 'secret-password', password);
      await tester.tap(button('Send sealed'));
      await settle(tester);
      expect(out, hasLength(1));
      expect('${out.single}', isNot(contains(password)));
      expect(platform.where((line) => line.contains(password)), isEmpty);
      // The sealed answer opens, for the host only, to the typed value.
      final reply = AcpSecretRequestReply.fromJson(jsonDecode(out.single as String), (name) => name != 'user') as AcpSecretAccepted;
      final opened = await tester.runAsync(() => receiver.openValues(hostChecks(), frontend.id, reply.plain, reply.sealed, reply.auth, now)) as AcpOpened;
      expect(opened.secrets['password']!.text, password);
      // And a card that cannot be verified never produces one.
      final (other, _) = await tester.runAsync(() => asked(as: stranger, id: 'r2')) as (AcpSecretRequestParams, AcpSecretReceiver);
      final refused = <AcpSecretRequestReply>[];
      await tester.pumpWidget(host(SecretRequestCard(request: other, environment: environment(), onReply: refused.add)));
      await settle(tester);
      expect(find.byKey(const ValueKey('secret-password')), findsNothing);
      expect(find.widgetWithText(FilledButton, 'Send sealed'), findsNothing);
    });

    testWidgets('Decline and Cancel answer without values, and only once', (tester) async {
      bigScreen(tester);
      final (params, _) = await tester.runAsync(asked) as (AcpSecretRequestParams, AcpSecretReceiver);
      final replies = <AcpSecretRequestReply>[];
      await tester.pumpWidget(host(SecretRequestCard(request: params, environment: environment(), onReply: replies.add)));
      await settle(tester);
      await type(tester, 'secret-password', password);
      await tester.tap(find.text('Decline'));
      await tester.tap(find.text('Cancel'));
      expect(replies.single, isA<AcpSecretDeclined>());
      expect(jsonEncode(replies.single.toJson()), '{"action":"decline"}');
    });

    testWidgets('a request from an identity that is not the host\'s, though known, is refused with nothing to fill', (tester) async {
      bigScreen(tester);
      final (params, _) = await tester.runAsync(() => asked(as: stranger)) as (AcpSecretRequestParams, AcpSecretReceiver);
      final replies = <AcpSecretRequestReply>[];
      await tester.pumpWidget(host(SecretRequestCard(request: params, environment: environment(), onReply: replies.add)));
      await settle(tester);
      expect(find.textContaining('cannot be verified'), findsOneWidget);
      expect(find.byType(TextField), findsNothing);
      await tester.tap(find.text('Cancel'));
      expect(replies.single, isA<AcpSecretCancelled>());
    });

    testWidgets('a form changed on the way (the secret mark removed) fails its binding and is not drawn', (tester) async {
      bigScreen(tester);
      final (params, _) = await tester.runAsync(asked) as (AcpSecretRequestParams, AcpSecretReceiver);
      final changed = AcpSecretRequestParams(
        context: AcpSecretContext(sessionId: 's1', requestId: 'r1', message: params.context.message, schema: schema.replaceAll('"writeOnly":true,', '')),
        recipient: params.recipient,
      );
      await tester.pumpWidget(host(SecretRequestCard(request: changed, environment: environment(), onReply: (_) {})));
      await settle(tester);
      expect(find.byType(TextField), findsNothing);
      expect(find.textContaining('cannot be verified'), findsOneWidget);
    });

    testWidgets('what the agent wrote cannot carry a control character into the card', (tester) async {
      bigScreen(tester);
      final context = AcpSecretContext(sessionId: 's1', requestId: 'r3', message: 'Pay\u202e now', schema: schema);
      final receiver = await tester.runAsync(() => AcpSecretReceiver.create(AcpSecretMethod.request, context, hostProver, frontend.id, now)) as AcpSecretReceiver;
      await tester.pumpWidget(host(SecretRequestCard(request: AcpSecretRequestParams(context: context, recipient: receiver.recipient), environment: environment(), agent: 'Cla\u202eude', onReply: (_) {})));
      await settle(tester);
      expect(find.textContaining('\u202e'), findsNothing);
    });
  });

  group('SecretShowCard', () {
    const showSchema = '{"type":"object","properties":{"label":{"type":"string","title":"Label"},"key":{"type":"string","title":"API key","writeOnly":true}}}';
    const plainValues = '{"label":"ci"}';

    AcpSecretShowOpen opening({String id = 'w1', String values = plainValues}) =>
        AcpSecretShowOpen(context: AcpSecretContext(sessionId: 's1', requestId: id, message: 'Your new API key', schema: showSchema, values: values));

    /// The host's second step: the collector checks the frontend's key, seals the value and binds the answer.
    Future<AcpSecretShowDeliver> delivered(AcpSecretShowOpened ready, AcpSecretShowOpen open, {String value = token, String? id, AcpSecretProver? as}) async {
      final collector = await hostChecks().collector(frontend.id, AcpSecretMethod.show, open.context, ready.recipient!, now);
      final sealed = {'key': await collector.seal('key', value)};
      final auth = await collector.authenticate(as ?? hostProver, open.context.values!, sealed, now);
      return AcpSecretShowDeliver(sessionId: 's1', requestId: id ?? open.context.requestId, sealed: sealed, auth: auth);
    }

    Future<void> shown(WidgetTester tester, AcpSecretShowOpen open, ValueNotifier<AcpSecretShowDeliver?> deliver, List<AcpSecretShowOpened> readied, List<bool> done, {Future<void> Function(String)? copy}) async {
      await tester.pumpWidget(host(SecretShowCard(open: open, environment: environment(), deliver: deliver, onReady: readied.add, onDone: done.add, agent: 'Claude', copy: copy)));
      await settle(tester);
    }

    testWidgets('asks before it shows anything, and never the value before the person says so', (tester) async {
      bigScreen(tester);
      final deliver = ValueNotifier<AcpSecretShowDeliver?>(null);
      final readied = <AcpSecretShowOpened>[];
      await shown(tester, opening(), deliver, readied, []);
      expect(find.text('Claude wants to show you a secret'), findsOneWidget);
      expect(find.text('Your new API key'), findsOneWidget);
      expect(find.text('API key'), findsOneWidget);
      expect(readied, isEmpty);
      await tester.tap(find.text('Decline'));
      expect(readied.single.action, 'decline');
    });

    testWidgets('Show answers with the card\'s own key; the host\'s sealed value arrives masked and opens on request, copy on demand, gone on Done', (tester) async {
      bigScreen(tester);
      final copied = <String>[];
      final deliver = ValueNotifier<AcpSecretShowDeliver?>(null);
      final readied = <AcpSecretShowOpened>[];
      final done = <bool>[];
      final open = opening();
      await shown(tester, open, deliver, readied, done, copy: (text) async => copied.add(text));
      await tester.tap(find.text('Show'));
      await settle(tester);
      expect(readied.single.action, 'ready');
      expect(find.text('Waiting for the values from the host.'), findsOneWidget);
      deliver.value = await tester.runAsync(() => delivered(readied.single, open));
      await settle(tester);
      expect(find.byKey(const ValueKey('masked-key')), findsOneWidget);
      expect(find.textContaining(token), findsNothing);
      expect(find.text('ci'), findsOneWidget);
      await tester.tap(find.byKey(const ValueKey('reveal-key')));
      await tester.pump();
      expect(find.text(token), findsOneWidget);
      expect(copied, isEmpty);
      await tester.tap(find.byKey(const ValueKey('copy-key')));
      await tester.pump();
      expect(copied, [token]);
      await tester.tap(find.text('Done'));
      await tester.pump();
      expect(done, [true]);
      expect(find.textContaining(token), findsNothing);
      expect(find.byKey(const ValueKey('reveal-key')), findsNothing);
      expect(find.text('Closed. The values were cleared from this device.'), findsOneWidget);
    });

    testWidgets('a value delivered before the card was listening is still shown, once', (tester) async {
      bigScreen(tester);
      final deliver = ValueNotifier<AcpSecretShowDeliver?>(null);
      final readied = <AcpSecretShowOpened>[];
      final open = opening();
      await shown(tester, open, deliver, readied, []);
      await tester.tap(find.text('Show'));
      await settle(tester);
      final one = await tester.runAsync(() => delivered(readied.single, open)) as AcpSecretShowDeliver;
      deliver.value = one;
      deliver.value = one;
      await settle(tester);
      expect(find.byKey(const ValueKey('masked-key')), findsOneWidget);
    });

    testWidgets('leaving the card clears it: the value is in no widget afterwards', (tester) async {
      bigScreen(tester);
      final deliver = ValueNotifier<AcpSecretShowDeliver?>(null);
      final readied = <AcpSecretShowOpened>[];
      final open = opening();
      await shown(tester, open, deliver, readied, []);
      await tester.tap(find.text('Show'));
      await settle(tester);
      deliver.value = await tester.runAsync(() => delivered(readied.single, open));
      await settle(tester);
      await tester.tap(find.byKey(const ValueKey('reveal-key')));
      await tester.pump();
      expect(find.text(token), findsOneWidget);
      await tester.pumpWidget(host(const SizedBox()));
      expect(find.textContaining(token), findsNothing);
    });

    testWidgets('a deliver for another request is refused, and the person is told it was cancelled', (tester) async {
      bigScreen(tester);
      final deliver = ValueNotifier<AcpSecretShowDeliver?>(null);
      final readied = <AcpSecretShowOpened>[];
      final done = <bool>[];
      final open = opening();
      await shown(tester, open, deliver, readied, done);
      await tester.tap(find.text('Show'));
      await settle(tester);
      deliver.value = await tester.runAsync(() => delivered(readied.single, open, id: 'other'));
      await settle(tester);
      expect(done, [false]);
      expect(find.textContaining('cannot be verified'), findsOneWidget);
      expect(find.textContaining(token), findsNothing);
    });

    testWidgets('a deliver bound by an identity that is not the host is refused, and shows nothing', (tester) async {
      bigScreen(tester);
      final deliver = ValueNotifier<AcpSecretShowDeliver?>(null);
      final readied = <AcpSecretShowOpened>[];
      final done = <bool>[];
      final open = opening();
      await shown(tester, open, deliver, readied, done);
      await tester.tap(find.text('Show'));
      await settle(tester);
      deliver.value = await tester.runAsync(() => delivered(readied.single, open, as: stranger));
      await settle(tester);
      expect(done, [false]);
      expect(find.textContaining('cannot be verified'), findsOneWidget);
    });
  });

  group('the registry', () {
    testWidgets('a transcript draws a secret entry as its card, and its text holds the message and never a value', (tester) async {
      bigScreen(tester);
      final (params, _) = await tester.runAsync(asked) as (AcpSecretRequestParams, AcpSecretReceiver);
      final entry = SecretRequestEntry(request: params, onReply: (_) {}, at: t0, agent: 'Claude');
      expect(entry.text, 'Secret requested: The deploy needs your registry login');
      await tester.pumpWidget(host(ChatTranscript(entries: [user('Deploy', 0), entry], renderers: secretRenderers(environment()))));
      await settle(tester);
      expect(find.byKey(const Key('secret-request-card')), findsOneWidget);
      expect(find.byKey(const ValueKey('secret-password')), findsOneWidget);
      // Another entry is left to the package's own drawing.
      expect(find.text('Deploy'), findsOneWidget);
    });

    testWidgets('a show entry is drawn too, and its deliver notifier reaches the card', (tester) async {
      bigScreen(tester);
      final open = AcpSecretShowOpen(context: AcpSecretContext(sessionId: 's1', requestId: 'w9', message: 'Your key', schema: '{"type":"object","properties":{"key":{"type":"string","writeOnly":true}}}', values: '{}'));
      final entry = SecretShowEntry(open: open, onReady: (_) {}, onDone: (_) {}, at: t0);
      await tester.pumpWidget(host(ChatTranscript(entries: [entry], renderers: secretRenderers(environment()))));
      await settle(tester);
      expect(find.byKey(const Key('secret-show-card')), findsOneWidget);
      expect(entry.text, 'Secret shown: Your key');
    });
  });
}
