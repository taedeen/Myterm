import 'package:flutter/material.dart';

import '../app_state.dart';
import 'connections_screen.dart';
import 'settings_screen.dart';
import 'vault_screen.dart';

/// Bottom navigation: Vaults · Connections · Settings.
class HomeShell extends StatefulWidget {
  const HomeShell({super.key, required this.state});
  final AppState state;

  @override
  State<HomeShell> createState() => _HomeShellState();
}

class _HomeShellState extends State<HomeShell> {
  int _tab = 0;

  @override
  Widget build(BuildContext context) {
    final pages = [
      VaultScreen(state: widget.state),
      ConnectionsScreen(state: widget.state),
      SettingsScreen(state: widget.state),
    ];
    return Scaffold(
      body: SafeArea(child: IndexedStack(index: _tab, children: pages)),
      bottomNavigationBar: NavigationBar(
        selectedIndex: _tab,
        onDestinationSelected: (i) => setState(() => _tab = i),
        destinations: [
          const NavigationDestination(icon: Icon(Icons.inventory_2_outlined), label: 'Vaults'),
          NavigationDestination(
            icon: Badge(
              isLabelVisible: widget.state.sessions.isNotEmpty,
              label: Text('${widget.state.sessions.length}'),
              child: const Icon(Icons.terminal),
            ),
            label: 'Connections',
          ),
          const NavigationDestination(icon: Icon(Icons.settings), label: 'Settings'),
        ],
      ),
    );
  }
}
