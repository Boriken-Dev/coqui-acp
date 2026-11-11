import 'package:coqui_acp/coqui_acp.dart' show AcpPermissionOption;

import 'limits.dart';
import 'text.dart';

/// One value an option can take.
final class Choice {
  const Choice(this.value, this.label);
  final String value;
  final String label;
}

/// Something the agent lets a person choose, such as its model or its mode.
final class ConfigOption {
  const ConfigOption({
    required this.id,
    required this.label,
    required this.current,
    required this.choices,
    this.isMode = false,
    this.category,
    this.defaultLabel,
  });
  final String id;
  final String label;
  final String current;
  final List<Choice> choices;

  /// What ACP says it is, such as `model`, `thought_level` or `mode`, when the agent says.
  final String? category;

  /// What taking no choice stands for, such as `Default (Opus 5.5)`, when a provider says.
  final String? defaultLabel;

  /// Set with `session/set_mode` rather than `session/set_config_option`.
  final bool isMode;

  /// This option with [value] as its current one.
  ConfigOption withCurrent(String value) => ConfigOption(
    id: id,
    label: label,
    current: value,
    choices: choices,
    isMode: isMode,
    category: category,
    defaultLabel: defaultLabel,
  );

  /// This option as the update that carries it.
  Map<String, Object?> toJson() => {
    'id': id,
    'name': label,
    'category': ?(isMode ? 'mode' : category),
    'currentValue': current,
    'options': [
      for (final choice in choices)
        {'value': choice.value, 'name': choice.label},
    ],
  };
}

Iterable<Object?> _flatChoices(List<dynamic> raw) sync* {
  for (final entry in raw) {
    // A group holds its own list of choices.
    if (entry is Map && entry['options'] is List) {
      yield* (entry['options'] as List);
    } else {
      yield entry;
    }
  }
}

/// The options of a `configOptions` list, skipping any that are not ACP's shape or whose current value is not among their
/// choices. Null when [raw] is not a list.
List<ConfigOption>? parseConfigOptions(Object? raw, ChatLimits limits) {
  if (raw is! List) return null;
  final parsed = <ConfigOption>[];
  for (final entry in raw.take(limits.configOptions)) {
    // ACP v2 calls it `configId`, and v1 `id`.
    final id = entry is Map ? entry['configId'] ?? entry['id'] : null;
    if (entry is! Map ||
        id is! String ||
        entry['currentValue'] is! String ||
        entry['options'] is! List) {
      continue;
    }
    final choices = _flatChoices(entry['options'] as List)
        .take(limits.choices)
        .whereType<Map>()
        .where((choice) => choice['value'] is String)
        .map(
          (choice) => Choice(
            choice['value'] as String,
            shortText(choice['name'], limits) ?? choice['value'] as String,
          ),
        )
        .toList();
    final current = entry['currentValue'] as String;
    if (!choices.any((choice) => choice.value == current)) continue;
    parsed.add(
      ConfigOption(
        id: id,
        label: shortText(entry['name'], limits) ?? id,
        current: current,
        choices: choices,
        category: shortText(entry['category'], limits),
      ),
    );
  }
  return parsed;
}

/// ACP v1's `modes` as an option; null when it is not a mode state or its current mode is not among those offered.
ConfigOption? parseModes(Object? raw, ChatLimits limits) {
  if (raw is! Map ||
      raw['currentModeId'] is! String ||
      raw['availableModes'] is! List) {
    return null;
  }
  final choices = (raw['availableModes'] as List)
      .take(limits.choices)
      .whereType<Map>()
      .where((mode) => mode['id'] is String)
      .map(
        (mode) => Choice(
          mode['id'] as String,
          shortText(mode['name'], limits) ?? mode['id'] as String,
        ),
      )
      .toList();
  final current = raw['currentModeId'] as String;
  if (!choices.any((choice) => choice.value == current)) return null;
  return ConfigOption(
    id: 'mode',
    label: 'Mode',
    current: current,
    choices: choices,
    isMode: true,
  );
}

/// Where an agent's choice belongs: its session mode, its model, its reasoning effort, or elsewhere.
enum OptionRole { mode, model, effort, other }

/// The role of [option]: by its ACP `category` where the agent gives one, else by its id.
OptionRole roleOf(ConfigOption option) {
  if (option.isMode) return OptionRole.mode;
  switch (option.category) {
    case 'mode':
      return OptionRole.mode;
    case 'model':
      return OptionRole.model;
    case 'thought_level':
      return OptionRole.effort;
    case final String _:
      return OptionRole.other;
  }
  return switch (option.id) {
    'mode' => OptionRole.mode,
    'model' => OptionRole.model,
    'effort' || 'reasoning_effort' || 'thought_level' => OptionRole.effort,
    _ => OptionRole.other,
  };
}

/// The first of [options] in [role].
ConfigOption? optionIn(List<ConfigOption> options, OptionRole role) =>
    options.where((option) => roleOf(option) == role).firstOrNull;

/// The mode option among [options].
ConfigOption? modeOptionIn(List<ConfigOption> options) =>
    optionIn(options, OptionRole.mode);

/// A choice made while a turn runs: it goes to the agent before the next prompt, to the option of the same id, else to one in the
/// same [role] that offers [value].
final class PendingChoice {
  const PendingChoice(this.id, this.role, this.value);
  final String id;
  final OptionRole role;
  final String value;
}

/// The ids an agent may give a mode that asks for nothing: the Rust translators' own, and the names Claude Code and others use.
const Set<String> _bypassIds = {
  'bypass',
  'bypasspermissions',
  'full_access',
  'full-access',
  'fullaccess',
  'yolo',
};

/// Whether a mode of [value], or labelled [label], asks for nothing.
bool isBypassMode(String value, [String? label]) =>
    _bypassIds.contains(value.toLowerCase()) ||
    label?.toLowerCase() == 'bypass';

/// The id of the bypass a chat provides itself, by answering requests, for an agent that has no mode that asks for nothing.
const String ownBypass = 'bypass';

/// The option a request in bypass is answered with: the first that allows once, else the first that allows always. Null when it
/// offers neither: that request is the person's.
AcpPermissionOption? bypassAnswer(List<AcpPermissionOption> options) =>
    options.where((option) => option.kind == 'allow_once').firstOrNull ??
    options.where((option) => option.kind == 'allow_always').firstOrNull;
