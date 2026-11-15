import 'package:flutter/material.dart';

import '../app_state.dart';
import 'terminal_screen.dart';

/// Open sessions. They keep running while you use the rest of the app;
/// tap one to return to its terminal.
class ConnectionsScreen extends StatelessWidget {
  const ConnectionsScreen({super.key, required this.state});
  final AppState state;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: state,
      builder: (context, _) {
        if (state.sessions.isEmpty) {
          return const Center(child: Text('No open connections. Open a host from Vaults → Hosts.'));
        }
        return ListView(children: [
          for (final s in state.sessions)
            ListenableBuilder(
              listenable: s,
              builder: (context, _) => Card(
                child: ListTile(
                  leading: Icon(Icons.terminal, color: s.isLive ? const Color(0xFF2ECC71) : Colors.grey),
                  title: Text(s.host.label),
                  subtitle: Text([
                    '${s.host.username.isEmpty ? '' : '${s.host.username}@'}${s.host.address}',
                    if (s.isLive) 'Active' else if (s.status.isNotEmpty) s.status,
                  ].join(' · ')),
                  trailing: Row(mainAxisSize: MainAxisSize.min, children: [
                    if (s.ended)
                      IconButton(tooltip: 'Reconnect', icon: const Icon(Icons.refresh), onPressed: () => s.connect()),
                    IconButton(tooltip: 'Close', icon: const Icon(Icons.close), onPressed: () => state.closeSession(s)),
                  ]),
                  onTap: () => showSession(context, state, s),
                ),
              ),
            ),
        ]);
      },
    );
  }
}
