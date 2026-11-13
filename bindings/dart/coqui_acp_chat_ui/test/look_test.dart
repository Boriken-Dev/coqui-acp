import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  final dark = ColorScheme.fromSeed(seedColor: const Color(0xFF3366CC), brightness: Brightness.dark);
  final light = ColorScheme.fromSeed(seedColor: const Color(0xFF3366CC));

  test('every token defaults from the colour scheme', () {
    final look = ChatLook.fromScheme(dark);
    expect(look.bg, dark.surface);
    expect(look.text, dark.onSurface);
    expect(look.muted, dark.onSurfaceVariant);
    expect(look.accent, dark.primary);
    expect(look.border, dark.outlineVariant);
    expect(look.danger, dark.error);
    expect(look.density, ChatDensity.normal);
    expect(look.codeFont, 'monospace');
  });

  test('the roles a preset does not give follow the tokens', () {
    final look = ChatLook.fromScheme(dark);
    expect(look.user, look.raised);
    expect(look.agent, look.surface);
    expect(look.composer, look.raised);
    final preset = look.copyWith(raised: const Color(0xFF123456));
    expect(preset.user, const Color(0xFF123456));
    // A role a preset gives is its own.
    expect(preset.copyWith(user: const Color(0xFF654321)).user, const Color(0xFF654321));
    expect(preset.copyWith(user: const Color(0xFF654321)).raised, const Color(0xFF123456));
  });

  test('added and removed are the states over the page', () {
    final look = ChatLook.fromScheme(dark);
    expect(look.added, Color.alphaBlend(look.success.withValues(alpha: 0.25), look.bg));
    expect(look.removed, Color.alphaBlend(look.danger.withValues(alpha: 0.25), look.bg));
  });

  test('copyWith keeps what it is not told to change, density and font included', () {
    final look = ChatLook.fromScheme(light, messageWidth: 900).copyWith(density: ChatDensity.verbose, codeFont: 'Menlo', userPrefix: '> ');
    expect(look.density, ChatDensity.verbose);
    expect(look.codeFont, 'Menlo');
    expect(look.userPrefix, '> ');
    expect(look.messageWidth, 900);
    expect(look.accent, light.primary);
  });

  test('lerp blends colours and sizes, and takes switches from the nearer', () {
    final a = ChatLook.fromScheme(dark, radius: 0, density: ChatDensity.verbose);
    final b = ChatLook.fromScheme(light, radius: 10);
    final mid = a.lerp(b, 0.5);
    expect(mid.radius, 5);
    expect(mid.bg, Color.lerp(a.bg, b.bg, 0.5));
    expect(a.lerp(b, 0.25).density, ChatDensity.verbose);
    expect(a.lerp(b, 0.75).density, ChatDensity.normal);
    expect(a.lerp(null, 0.5), same(a));
  });

  test('densities have names a settings file can use', () {
    expect(ChatDensity.named('thinking'), ChatDensity.thinking);
    expect(ChatDensity.named('nope'), isNull);
    expect([for (final density in ChatDensity.values) density.label], ['Normal', 'Thinking', 'Verbose']);
  });

  testWidgets('context.chatLook is the theme extension, else one made from the scheme', (tester) async {
    late ChatLook seen;
    final own = ChatLook.fromScheme(dark).copyWith(accent: const Color(0xFFFF0000));
    await tester.pumpWidget(MaterialApp(
      theme: ThemeData(colorScheme: dark, extensions: [own]),
      home: Builder(builder: (context) {
        seen = context.chatLook;
        return const SizedBox();
      }),
    ));
    expect(seen.accent, const Color(0xFFFF0000));
    await tester.pumpWidget(MaterialApp(
      theme: ThemeData(colorScheme: dark),
      home: Builder(builder: (context) {
        seen = context.chatLook;
        return const SizedBox();
      }),
    ));
    await tester.pumpAndSettle();
    expect(seen.accent, dark.primary);
  });
}
