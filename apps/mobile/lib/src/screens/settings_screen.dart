import 'package:flutter/material.dart';

import '../app_state.dart';

class SettingsScreen extends StatelessWidget {
  const SettingsScreen({super.key, required this.state});
  final AppState state;

  @override
  Widget build(BuildContext context) {
    final keybars = state.core.builtinKeybars();
    return ListenableBuilder(listenable: state, builder: (context, _) => ListView(children: [
      const Padding(
        padding: EdgeInsets.all(24),
        child: Text('Settings', style: TextStyle(fontSize: 30, fontWeight: FontWeight.bold)),
      ),
      Card(
        child: ListTile(
          leading: const Icon(Icons.keyboard),
          title: const Text('Default key bar'),
          trailing: DropdownButton<String>(
            value: state.keybarProfileId,
            items: [for (final k in keybars) DropdownMenuItem(value: k.id, child: Text(k.name))],
            onChanged: (v) {
              if (v != null) {
                state.keybarProfileId = v;
              }
            },
          ),
        ),
      ),
      Card(
        child: ListTile(
          leading: const Icon(Icons.receipt_long),
          title: const Text('Audit log'),
          subtitle: const Text('Connections, typed credentials and host key decisions (no secret values)'),
          onTap: () async {
            final log = await state.core.exportAuditJsonl();
            if (!context.mounted) return;
            showModalBottomSheet(
              context: context,
              isScrollControlled: true,
              builder: (_) => DraggableScrollableSheet(
                expand: false,
                builder: (_, c) => SingleChildScrollView(
                  controller: c,
                  padding: const EdgeInsets.all(16),
                  child: SelectableText(log.isEmpty ? 'No events yet.' : log,
                      style: const TextStyle(fontFamily: 'monospace', fontSize: 11)),
                ),
              ),
            );
          },
        ),
      ),
      Card(
        child: ListTile(
          leading: const Icon(Icons.lock),
          title: const Text('Lock vault now'),
          onTap: state.lock,
        ),
      ),
      const Padding(
        padding: EdgeInsets.all(24),
        child: Text('MyTerm 0.1.0 · Android preview', style: TextStyle(color: Colors.white54)),
      ),
    ]));
  }
}
