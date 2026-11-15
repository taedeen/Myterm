import 'package:flutter/material.dart';

import '../rust/api.dart';

/// Autocomplete chips for the line being typed (spec 3.3).
class SuggestionBar extends StatefulWidget {
  const SuggestionBar({super.key, required this.suggestions, required this.onAccept, this.selected});

  final List<SuggestionDto> suggestions;

  /// Highlighted with Tab; Enter runs it.
  final int? selected;
  final void Function(SuggestionDto) onAccept;

  IconData _icon(String source) => switch (source) {
        'Snippet' => Icons.data_object,
        'HostHistory' => Icons.history,
        'RelatedHistory' => Icons.device_hub,
        'Remote' => Icons.cloud_outlined,
        _ => Icons.auto_awesome,
      };

  @override
  State<SuggestionBar> createState() => _SuggestionBarState();
}

class _SuggestionBarState extends State<SuggestionBar> {
  final _selectedKey = GlobalKey();

  @override
  Widget build(BuildContext context) {
    final suggestions = widget.suggestions;
    final selected = widget.selected;
    if (suggestions.isEmpty) return const SizedBox.shrink();
    if (selected != null) {
      // Keep the highlighted chip in view as Tab moves along.
      WidgetsBinding.instance.addPostFrameCallback((_) {
        final ctx = _selectedKey.currentContext;
        if (ctx != null) Scrollable.ensureVisible(ctx, alignment: 0.5, duration: const Duration(milliseconds: 120));
      });
    }
    return SizedBox(
      height: 44,
      child: ListView(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: 8),
        children: [
          for (final (i, s) in suggestions.indexed)
            Padding(
              padding: const EdgeInsets.only(right: 6),
              child: ActionChip(
                key: i == selected ? _selectedKey : null,
                backgroundColor: i == selected ? const Color(0xFF173A2F) : null,
                side: i == selected ? const BorderSide(color: Color(0xFF2ECC71)) : null,
                avatar: Icon(widget._icon(s.source), size: 16),
                label: Text(s.line, style: const TextStyle(fontFamily: 'monospace')),
                onPressed: () => widget.onAccept(s),
              ),
            ),
        ],
      ),
    );
  }
}
