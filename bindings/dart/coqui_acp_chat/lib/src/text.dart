import 'limits.dart';

final _unprintable = RegExp(
  r'[\u{0}-\u{1f}\u{7f}-\u{9f}\u{200e}\u{200f}\u{2028}-\u{202e}\u{2066}-\u{2069}]',
  unicode: true,
);

/// [value] as one short line of text a person may be shown: control and direction-changing characters become spaces, since they can
/// reorder or hide what surrounds them, and it is cut at [ChatLimits.shortText]. Null when it is not a string or is blank.
String? shortText(Object? value, ChatLimits limits) {
  if (value is! String) return null;
  final clean = value.replaceAll(_unprintable, ' ').trim();
  if (clean.isEmpty) return null;
  return clean.length > limits.shortText
      ? '${clean.substring(0, limits.shortText)}…'
      : clean;
}

/// The last [limit] characters of [text].
String boundedTail(String text, int limit) =>
    text.length > limit ? text.substring(text.length - limit) : text;

/// The first [limit] characters of [text], with a `…` line after them when it was cut.
String boundedHead(String text, int limit) =>
    text.length > limit ? '${text.substring(0, limit)}\n…' : text;
