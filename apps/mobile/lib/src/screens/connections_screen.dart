import 'package:flutter/material.dart';

import '../app_state.dart';
import 'terminal_screen.dart';

/// Live sessions. Tapping one opens a new terminal for that host
/// (session re-attach to an existing view is a later milestone).
class ConnectionsScreen extends StatelessWidget {
  const ConnectionsScreen({super.key, required this.state});
  final AppState state;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: state,
      builder: (context, _) {
        if (state.sessions.isEmpty) {
          return const Center(child: Text('No live connections. Open a host from Vaults → Hosts.'));
        }
        return ListView(children: [
          for (final s in state.sessions)
            Card(
              child: ListTile(
                leading: const Icon(Icons.terminal, color: Color(0xFF2ECC71)),
                title: Text(s.host.label),
                subtitle: Text('${s.host.username}@${s.host.address} · session ${s.id}'),
                trailing: IconButton(
                  icon: const Icon(Icons.close),
                  onPressed: () => state.core.disconnect(sessionId: s.id),
                ),
                onTap: () => openTerminal(context, state, s.host),
              ),
            ),
        ]);
      },
    );
  }
}
