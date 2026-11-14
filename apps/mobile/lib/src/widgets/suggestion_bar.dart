import 'package:flutter/material.dart';

import '../rust/api.dart';

/// Autocomplete chips for the line being typed (spec 3.3).
class SuggestionBar extends StatelessWidget {
  const SuggestionBar({super.key, required this.suggestions, required this.onAccept});

  final List<SuggestionDto> suggestions;
  final void Function(SuggestionDto) onAccept;

  IconData _icon(String source) => switch (source) {
        'Snippet' => Icons.data_object,
        'HostHistory' => Icons.history,
        'RelatedHistory' => Icons.device_hub,
        'Remote' => Icons.cloud_outlined,
        _ => Icons.auto_awesome,
      };

  @override
  Widget build(BuildContext context) {
    if (suggestions.isEmpty) return const SizedBox.shrink();
    return SizedBox(
      height: 44,
      child: ListView(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: 8),
        children: [
          for (final s in suggestions)
            Padding(
              padding: const EdgeInsets.only(right: 6),
              child: ActionChip(
                avatar: Icon(_icon(s.source), size: 16),
                label: Text(s.line, style: const TextStyle(fontFamily: 'monospace')),
                onPressed: () => onAccept(s),
              ),
            ),
        ],
      ),
    );
  }
}
