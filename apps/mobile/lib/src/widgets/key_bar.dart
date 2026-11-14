import 'dart:typed_data';

import 'package:flutter/material.dart';

import '../rust/api.dart';

/// Swipeable rows of extra keys above the keyboard (spec 3.5 A).
class KeyBar extends StatefulWidget {
  const KeyBar({
    super.key,
    required this.profile,
    required this.onBytes,
    required this.onSnippet,
    required this.onTypePassword,
  });

  final KeybarDto profile;
  final void Function(Uint8List bytes) onBytes;
  final void Function(String snippetId) onSnippet;
  final void Function(String? credentialId) onTypePassword;

  @override
  State<KeyBar> createState() => _KeyBarState();
}

class _KeyBarState extends State<KeyBar> {
  final _page = PageController();

  @override
  void dispose() {
    _page.dispose();
    super.dispose();
  }

  void _press(KeyDto key) {
    switch (key.action) {
      case KeyActionDto_Bytes(:final field0):
        widget.onBytes(field0);
      case KeyActionDto_Snippet(:final field0):
        widget.onSnippet(field0);
      case KeyActionDto_TypePassword(:final field0):
        widget.onTypePassword(field0);
    }
  }

  @override
  Widget build(BuildContext context) {
    const green = Color(0xFF2ECC71);
    return Container(
      height: 39, // was 52
      color: const Color(0xFF1B1E2E),
      child: PageView(
        controller: _page,
        children: [
          for (final row in widget.profile.rows)
            ListView(
              scrollDirection: Axis.horizontal,
              padding: const EdgeInsets.symmetric(horizontal: 2),
              children: [
                for (final key in row)
                  TextButton(
                      onPressed: () => _press(key),
                      style: TextButton.styleFrom(
                        foregroundColor: green,
                        minimumSize: const Size(30, 39),
                        padding: const EdgeInsets.symmetric(horizontal: 6),
                        visualDensity: VisualDensity.compact,
                        tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                      ),
                      child: key.action is KeyActionDto_TypePassword
                          ? const Tooltip(message: 'Type password', child: Icon(Icons.key, size: 20))
                          : Text(key.label, style: const TextStyle(fontSize: 15)),
                  ),
              ],
            ),
        ],
      ),
    );
  }
}
