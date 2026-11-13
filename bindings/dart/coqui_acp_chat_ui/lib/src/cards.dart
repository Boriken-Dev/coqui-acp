import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'look.dart';

const ChatLimits _limits = ChatLimits();

/// [value] as one short line for a person, or null: what an agent called something, with nothing in it that could hide the rest.
String? shortLine(Object? value) => shortText(value, _limits);

/// Where an option goes among the answers: refusals first, then what the chat does not know, then what allows; once before always.
int _rank(AcpPermissionOption option) => switch (option.kind) {
  'reject_once' => 0,
  'reject_always' => 1,
  'allow_once' => 3,
  'allow_always' => 4,
  _ => 2,
};

/// The words for an option the agent sent without a name.
String _kindLabel(AcpPermissionOption option) => switch (option.kind) {
  'allow_once' => 'Allow once',
  'allow_always' => 'Allow always',
  'reject_once' => 'Refuse once',
  'reject_always' => 'Refuse always',
  _ => shortLine(option.kind) ?? shortLine(option.optionId) ?? 'An answer',
};

/// One of the agent's answers, under the agent's own name for it.
class PermissionChoice extends StatelessWidget {
  const PermissionChoice({super.key, required this.option, required this.onPressed});

  final AcpPermissionOption option;
  final VoidCallback onPressed;

  String get label => shortLine(option.name) ?? _kindLabel(option);

  @override
  Widget build(BuildContext context) {
    final text = Text(label, maxLines: 2, overflow: TextOverflow.ellipsis);
    return switch (option.kind) {
      'allow_once' => FilledButton(onPressed: onPressed, child: text),
      'allow_always' => FilledButton.tonal(onPressed: onPressed, child: text),
      'reject_once' || 'reject_always' => TextButton(onPressed: onPressed, child: text),
      _ => OutlinedButton(onPressed: onPressed, child: text),
    };
  }
}

/// What an agent is waiting on, with the answers it offers. It stays until the person answers.
///
/// A host adds to it without a fork: [notes] are lines under the question (what a remembered rule would do), [extraAnswers] are
/// buttons beside the agent's own (an "Always allow here" that the host answers itself).
class PermissionCard extends StatelessWidget {
  const PermissionCard({
    super.key,
    required this.agent,
    required this.title,
    required this.options,
    required this.onChoose,
    this.detail,
    this.bypassPassed = false,
    this.forPerson = false,
    this.more = 0,
    this.onCancel,
    this.notes = const [],
    this.extraAnswers = const [],
    this.host = 'This app',
    this.rememberedBy,
  });

  /// Narrower than this, the answers go under the question rather than beside it, which would leave its text a thin column.
  static const double besideFrom = 560;

  /// The agent's name, as it gave it.
  final String agent;
  final String title;

  /// What it would run or touch, when the agent said.
  final String? detail;
  final List<AcpPermissionOption> options;

  /// Whether Bypass was on and left this to the person, and whether it is a question or a plan, which Bypass never answers.
  final bool bypassPassed, forPerson;

  /// How many requests wait after this one.
  final int more;
  final ValueChanged<AcpPermissionOption> onChoose;

  /// Answers the request cancelled, offered when the agent gave no answer to choose.
  final VoidCallback? onCancel;
  final List<Widget> notes;
  final List<Widget> extraAnswers;

  /// What the chat calls itself in the card's sentences: "Notes cannot see or undo what [agent] keeps."
  final String host;

  /// Overrides the sentence about an "always" answer, which names [host]; null for the package's own.
  final String? rememberedBy;

  @override
  Widget build(BuildContext context) {
    final small = Theme.of(context).textTheme.labelSmall;
    final ordered = [...options]..sort((a, b) => _rank(a) - _rank(b));
    final remembers = ordered.any((option) => option.kind.endsWith('_always'));
    final name = shortLine(agent) ?? 'The agent';
    final asked = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text('$name wants to use a tool', style: Theme.of(context).textTheme.labelMedium),
        Text(title),
        if (detail?.isNotEmpty ?? false) _ToApprove(detail: detail!),
        if (remembers)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(
              rememberedBy ?? 'An "always" answer is remembered by $name, not by $host: $host cannot see or undo what it keeps.',
              style: small,
            ),
          ),
        for (final note in notes) Padding(padding: const EdgeInsets.only(top: 4), child: note),
        if (bypassPassed)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(
              forPerson
                  ? 'Bypass leaves a question or a plan to you, so this one is yours to answer.'
                  : 'Bypass answers only a request that offers to allow, so this one is yours to answer.',
              style: small?.copyWith(fontWeight: FontWeight.w600),
            ),
          ),
        if (ordered.isEmpty)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text('The agent offered no answer $host can give.', style: small),
          ),
        if (more > 0) Text('$more more after this', style: small),
      ],
    );
    final answers = Wrap(
      spacing: 8,
      runSpacing: 4,
      alignment: WrapAlignment.end,
      children: [
        for (final option in ordered) PermissionChoice(option: option, onPressed: () => onChoose(option)),
        ...extraAnswers,
        if (ordered.isEmpty && onCancel != null) TextButton(onPressed: onCancel, child: const Text('Cancel the request')),
      ],
    );
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
      child: Card(
        child: Padding(
          padding: const EdgeInsets.all(12),
          child: LayoutBuilder(builder: (context, constraints) {
            final beside = constraints.maxWidth >= besideFrom && ordered.length + extraAnswers.length <= 2;
            final top = Row(
              crossAxisAlignment: beside ? CrossAxisAlignment.center : CrossAxisAlignment.start,
              children: [
                const Icon(Icons.shield_outlined),
                const SizedBox(width: 12),
                Expanded(child: asked),
                if (beside) answers,
              ],
            );
            if (beside) return top;
            return Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [top, const SizedBox(height: 8), Align(alignment: Alignment.centerRight, child: answers)],
            );
          }),
        ),
      ),
    );
  }
}

/// What a tool would do, in a box of a fixed height. Whether it holds more than the box shows is measured at the width it has, so a
/// short edit spread over a few lines is not called long, and a long one is.
class _ToApprove extends StatelessWidget {
  const _ToApprove({required this.detail});

  final String detail;

  static const double shown = 120;

  @override
  Widget build(BuildContext context) {
    final style = TextStyle(fontFamily: context.chatLook.codeFont, fontSize: 12);
    return Padding(
      padding: const EdgeInsets.only(top: 4),
      child: LayoutBuilder(builder: (context, constraints) {
        final painter = TextPainter(
          text: TextSpan(text: detail, style: DefaultTextStyle.of(context).style.merge(style)),
          textDirection: Directionality.of(context),
          textScaler: MediaQuery.textScalerOf(context),
        )..layout(maxWidth: constraints.maxWidth);
        final long = painter.height > shown;
        painter.dispose();
        return Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            ConstrainedBox(
              constraints: const BoxConstraints(maxHeight: shown),
              child: SingleChildScrollView(child: SelectableText(detail, style: style)),
            ),
            if (long)
              Padding(
                padding: const EdgeInsets.only(top: 4),
                child: Text(
                  'This is long. Scroll to read all of it before you answer.',
                  style: Theme.of(context).textTheme.labelSmall?.copyWith(fontWeight: FontWeight.w600),
                ),
              ),
          ],
        );
      }),
    );
  }
}

/// Says, for as long as it lasts, that a conversation is in Bypass, with the one action that ends it. It is a status, in the
/// colours of the mode's pill, not an alarm.
class BypassBanner extends StatelessWidget {
  const BypassBanner({super.key, required this.onLeave});

  final VoidCallback onLeave;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Material(
      key: const Key('bypass-banner'),
      color: scheme.tertiaryContainer,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 6),
        child: Row(children: [
          Icon(Icons.fast_forward_outlined, color: scheme.onTertiaryContainer),
          const SizedBox(width: 12),
          Expanded(
            child: Text(
              'Bypass: each request that offers to allow is allowed without asking you.',
              style: TextStyle(color: scheme.onTertiaryContainer),
            ),
          ),
          TextButton(
            onPressed: onLeave,
            style: TextButton.styleFrom(foregroundColor: scheme.onTertiaryContainer),
            child: const Text('Leave Bypass'),
          ),
        ]),
      ),
    );
  }
}

/// The most of an agent's question shown, since it can send any amount.
const int maxQuestionText = 4000;

final _hidden = RegExp(r'[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u200e\u200f\u2028-\u202e\u2066-\u2069]');

/// An agent's text as plain data: line breaks kept, nothing that could reorder or hide what surrounds it.
String plainText(String text) {
  final clean = text.replaceAll('\r\n', '\n').replaceAll(_hidden, ' ');
  return clean.length > maxQuestionText ? '${clean.substring(0, maxQuestionText)}\n…' : clean;
}

/// An agent's question, as a form built from its schema. The person sends an answer, declines, or cancels; everything the agent
/// wrote is shown as text, never as markup or a link.
class QuestionCard extends StatefulWidget {
  const QuestionCard({
    super.key,
    required this.request,
    required this.agent,
    required this.onAnswer,
    this.more = 0,
    this.host = 'This app',
  });

  /// The question; a new one (not the same object) starts a new form.
  final AcpElicitation request;
  final String agent;
  final int more;
  final ValueChanged<AcpElicitationAnswer> onAnswer;
  final String host;

  @override
  State<QuestionCard> createState() => _QuestionCardState();
}

class _QuestionCardState extends State<QuestionCard> {
  /// What the person gave each field so far, by name; a field with no value is left out of the answer.
  final Map<String, Object> _values = {};
  final Map<String, TextEditingController> _texts = {};

  @override
  void initState() {
    super.initState();
    _start();
  }

  @override
  void didUpdateWidget(QuestionCard oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (!identical(widget.request, oldWidget.request)) {
      _clear();
      _start();
    }
  }

  /// The form of the question, at the values the agent gave as defaults.
  void _start() {
    for (final field in widget.request.fields) {
      final start = switch (field) {
        AcpTextField(:final defaultValue) => defaultValue,
        AcpChoiceField(:final defaultValue) => defaultValue,
        AcpChoicesField(:final defaultValue) => defaultValue,
        AcpNumberField(:final defaultValue) => defaultValue,
        AcpBooleanField(:final defaultValue) => defaultValue,
        AcpUnsupportedField() => null,
      };
      if (start != null) _values[field.name] = start;
      if (field is AcpTextField || field is AcpNumberField) {
        _texts[field.name] = TextEditingController(text: start == null ? '' : '$start');
      }
    }
  }

  void _clear() {
    for (final controller in _texts.values) {
      controller.dispose();
    }
    _texts.clear();
    _values.clear();
  }

  @override
  void dispose() {
    _clear();
    super.dispose();
  }

  /// Why the answer cannot be sent yet, or null when it can.
  String? get _problem {
    for (final field in widget.request.fields) {
      final value = _values[field.name];
      final label = shortLine(field.title) ?? shortLine(field.name) ?? 'A question';
      if (value == null) {
        if (field.required) {
          return field is AcpUnsupportedField ? '$label cannot be answered here, so only Decline or Cancel can answer this.' : '$label is required.';
        }
        continue;
      }
      switch (field) {
        case AcpTextField(:final minLength, :final maxLength):
          final length = (value as String).runes.length;
          if (minLength != null && length < minLength) return '$label needs at least $minLength characters.';
          if (maxLength != null && length > maxLength) return '$label takes at most $maxLength characters.';
        case AcpChoicesField(:final minItems, :final maxItems):
          final count = (value as List).length;
          if (minItems != null && count < minItems) return 'Choose at least $minItems for $label.';
          if (maxItems != null && count > maxItems) return 'Choose at most $maxItems for $label.';
        case AcpNumberField(:final minimum, :final maximum):
          if (minimum != null && (value as num) < minimum) return '$label is at least $minimum.';
          if (maximum != null && (value as num) > maximum) return '$label is at most $maximum.';
        default:
          break;
      }
    }
    return null;
  }

  Widget _field(AcpField field) {
    final title = shortLine(field.title) ?? shortLine(field.name) ?? 'A question';
    final description = field.description == null ? null : plainText(field.description!);
    final label = '$title${field.required ? ' *' : ''}';
    void set(Object? value) => setState(() => value == null ? _values.remove(field.name) : _values[field.name] = value);
    final key = ValueKey('question-${field.name}');
    switch (field) {
      case AcpTextField(:final format):
        return TextField(
          key: key,
          controller: _texts[field.name],
          decoration: InputDecoration(labelText: label, helperText: [?description, ?shortLine(format)].join(' · ').nullIfEmpty),
          onChanged: (text) => set(text.isEmpty ? null : text),
        );
      case AcpNumberField(:final integer):
        return TextField(
          key: key,
          controller: _texts[field.name],
          keyboardType: TextInputType.numberWithOptions(decimal: !integer, signed: true),
          inputFormatters: [FilteringTextInputFormatter.allow(RegExp(integer ? r'[-0-9]' : r'[-0-9.eE+]'))],
          decoration: InputDecoration(labelText: label, helperText: description),
          onChanged: (text) => set(integer ? int.tryParse(text) : num.tryParse(text)),
        );
      case AcpBooleanField():
        return SwitchListTile(
          key: key,
          contentPadding: EdgeInsets.zero,
          title: Text(label),
          subtitle: description == null ? null : Text(description),
          value: _values[field.name] == true,
          onChanged: set,
        );
      case AcpChoiceField(:final options):
        return _Labelled(label: label, description: description, child: Wrap(spacing: 6, runSpacing: 4, children: [
          for (final option in options)
            _Described(
              description: option.description,
              child: ChoiceChip(
                key: ValueKey('question-${field.name}-${option.value}'),
                label: Text(shortLine(option.title) ?? shortLine(option.value) ?? '?'),
                selected: _values[field.name] == option.value,
                onSelected: (on) => set(on ? option.value : null),
              ),
            ),
        ]));
      case AcpChoicesField(:final options):
        final chosen = [...(_values[field.name] as List<String>?) ?? const <String>[]];
        return _Labelled(label: label, description: description, child: Wrap(spacing: 6, runSpacing: 4, children: [
          for (final option in options)
            _Described(
              description: option.description,
              child: FilterChip(
                key: ValueKey('question-${field.name}-${option.value}'),
                label: Text(shortLine(option.title) ?? shortLine(option.value) ?? '?'),
                selected: chosen.contains(option.value),
                onSelected: (on) {
                  // What is chosen now, which a tap since this was built may have changed.
                  final chosen = [...(_values[field.name] as List<String>?) ?? const <String>[]];
                  on ? chosen.add(option.value) : chosen.remove(option.value);
                  // The answer keeps the agent's order.
                  final ordered = [for (final each in options) if (chosen.contains(each.value)) each.value];
                  set(ordered.isEmpty ? null : ordered);
                },
              ),
            ),
        ]));
      case AcpUnsupportedField(:final type):
        return _Labelled(
          label: label,
          description: description,
          child: Text('A question of a kind ${widget.host} cannot show (${shortLine(type) ?? 'no type'}), so it is left unanswered.',
              style: Theme.of(context).textTheme.labelSmall),
        );
    }
  }

  @override
  Widget build(BuildContext context) {
    final request = widget.request;
    final problem = _problem;
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
      child: Card(
        key: const Key('question-card'),
        child: ConstrainedBox(
          constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(context).height * 0.55),
          child: SingleChildScrollView(
            padding: const EdgeInsets.all(12),
            child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
              Row(children: [
                const Icon(Icons.help_outline),
                const SizedBox(width: 12),
                Expanded(child: Text('${shortLine(widget.agent) ?? 'The agent'} asks', style: theme.textTheme.labelMedium)),
              ]),
              if (request.title != null) Text(shortLine(request.title) ?? '', style: theme.textTheme.titleSmall),
              SelectableText(plainText(request.message)),
              if (request.description != null) Text(plainText(request.description!), style: theme.textTheme.bodySmall),
              for (final field in request.fields) Padding(padding: const EdgeInsets.only(top: 8), child: _field(field)),
              if (problem != null)
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: Text(problem, style: theme.textTheme.labelSmall?.copyWith(fontWeight: FontWeight.w600)),
                ),
              if (widget.more > 0) Text('${widget.more} more after this', style: theme.textTheme.labelSmall),
              const SizedBox(height: 8),
              Wrap(alignment: WrapAlignment.end, spacing: 8, children: [
                TextButton(onPressed: () => widget.onAnswer(const AcpElicitCancel()), child: const Text('Cancel')),
                TextButton(onPressed: () => widget.onAnswer(const AcpElicitDecline()), child: const Text('Decline')),
                FilledButton(
                  onPressed: problem == null ? () => widget.onAnswer(AcpElicitAccept(Map.of(_values))) : null,
                  child: const Text('Send answer'),
                ),
              ]),
            ]),
          ),
        ),
      ),
    );
  }
}

/// A choice, with the agent's description of it as a tooltip when it gave one.
class _Described extends StatelessWidget {
  const _Described({required this.description, required this.child});

  final String? description;
  final Widget child;

  @override
  Widget build(BuildContext context) => description == null ? child : Tooltip(message: plainText(description!), child: child);
}

class _Labelled extends StatelessWidget {
  const _Labelled({required this.label, required this.child, this.description});

  final String label;
  final String? description;
  final Widget child;

  @override
  Widget build(BuildContext context) => Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
    Text(label, style: Theme.of(context).textTheme.labelLarge),
    if (description != null) Text(description!, style: Theme.of(context).textTheme.bodySmall),
    const SizedBox(height: 4),
    child,
  ]);
}

extension on String {
  String? get nullIfEmpty => isEmpty ? null : this;
}

/// One message waiting for its turn, as the queue shows it.
class QueuedLine {
  const QueuedLine(this.key, this.text, {this.attachments = const []});

  /// Names the line across rebuilds.
  final Object key;
  final String text;

  /// The names of what goes with it.
  final List<String> attachments;
}

/// The messages written while the agent worked, in the order they go, each editable and removable. While the queue is held,
/// after a turn that was stopped or failed, it says so and waits for the person to send the next or clear them.
class QueuePanel extends StatelessWidget {
  const QueuePanel({
    super.key,
    required this.lines,
    required this.onEdit,
    required this.onRemove,
    required this.onClear,
    this.held,
    this.working = false,
    this.onSendNext,
    this.dialog,
  });

  final List<QueuedLine> lines;

  /// Why the queue waits for the person rather than going on; null while it goes on.
  final String? held;
  final bool working;
  final void Function(int index, String text) onEdit;
  final ValueChanged<int> onRemove;
  final VoidCallback onClear;

  /// Sends the next message of a held queue.
  final VoidCallback? onSendNext;

  /// Shows a dialog; [showDialog] when null. A host whose dialogs are all selectable passes its own.
  final Future<String?> Function(BuildContext context, WidgetBuilder builder)? dialog;

  Future<void> _edit(BuildContext context, int index) async {
    final show = dialog ?? (context, builder) => showDialog<String>(context: context, builder: builder);
    final text = await show(context, (_) => _EditQueued(text: lines[index].text));
    if (text != null) onEdit(index, text);
  }

  @override
  Widget build(BuildContext context) {
    final look = context.chatLook;
    final small = TextStyle(fontSize: 12, color: look.muted);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 6, 16, 0),
      child: Center(
        heightFactor: 1,
        child: ConstrainedBox(
          constraints: BoxConstraints(maxWidth: look.messageWidth + 100),
          child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, mainAxisSize: MainAxisSize.min, children: [
            Row(children: [
              Icon(held == null ? Icons.schedule : Icons.pause_circle_outline, size: 16, color: look.muted),
              const SizedBox(width: 6),
              Expanded(
                child: Text(
                  held == null
                      ? '${lines.length} queued: sent one at a time, as each turn ends'
                      : 'Held, since $held: nothing more is sent until you say',
                  style: small,
                ),
              ),
              if (held != null && !working && onSendNext != null) TextButton(onPressed: onSendNext, child: const Text('Send next')),
              TextButton(onPressed: onClear, child: const Text('Clear queue')),
            ]),
            ConstrainedBox(
              constraints: const BoxConstraints(maxHeight: 180),
              child: ListView(shrinkWrap: true, children: [
                for (final (index, line) in lines.indexed)
                  Container(
                    key: ObjectKey(line.key),
                    margin: const EdgeInsets.only(bottom: 4),
                    padding: const EdgeInsets.only(left: 12),
                    decoration: BoxDecoration(
                      border: Border.all(color: look.muted.withValues(alpha: 0.6)),
                      borderRadius: BorderRadius.circular(look.radius / 2),
                    ),
                    child: Row(children: [
                      Expanded(
                        child: Text(
                          [
                            line.text.trim(),
                            if (line.attachments.isNotEmpty) '(with ${line.attachments.join(', ')})',
                          ].where((part) => part.isNotEmpty).join(' '),
                          maxLines: 2,
                          overflow: TextOverflow.ellipsis,
                        ),
                      ),
                      IconButton(
                        tooltip: 'Edit queued message',
                        icon: const Icon(Icons.edit_outlined, size: 18),
                        onPressed: () => _edit(context, index),
                      ),
                      IconButton(tooltip: 'Remove queued message', icon: const Icon(Icons.close, size: 18), onPressed: () => onRemove(index)),
                    ]),
                  ),
              ]),
            ),
          ]),
        ),
      ),
    );
  }
}

class _EditQueued extends StatefulWidget {
  const _EditQueued({required this.text});

  final String text;

  @override
  State<_EditQueued> createState() => _EditQueuedState();
}

class _EditQueuedState extends State<_EditQueued> {
  late final _text = TextEditingController(text: widget.text);

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('Queued message'),
    content: SizedBox(
      width: 480,
      child: TextField(key: const Key('queued-text'), controller: _text, autofocus: true, minLines: 2, maxLines: 8),
    ),
    actions: [
      TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
      FilledButton(onPressed: () => Navigator.pop(context, _text.text), child: const Text('Save')),
    ],
  );
}

/// What the session has used, as the agent says, in one quiet line under the conversation; nothing when it said none.
class UsageFooter extends StatelessWidget {
  const UsageFooter({super.key, required this.text});

  final String? text;

  @override
  Widget build(BuildContext context) {
    final text = this.text;
    if (text == null) return const SizedBox.shrink();
    final look = context.chatLook;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 16),
      child: Text(text, style: TextStyle(fontSize: look.bodySize - 3, color: look.muted)),
    );
  }
}
