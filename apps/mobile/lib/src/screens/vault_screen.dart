import 'package:flutter/material.dart';

import '../app_state.dart';
import 'credentials_screen.dart';
import 'hardware_keys_screen.dart';
import 'hosts_screen.dart';
import 'snippets_screen.dart';

class VaultScreen extends StatelessWidget {
  const VaultScreen({super.key, required this.state});
  final AppState state;

  @override
  Widget build(BuildContext context) {
    Widget tile(IconData icon, String title, String? count, Widget Function() page) => Card(
          child: ListTile(
            contentPadding: const EdgeInsets.symmetric(horizontal: 20, vertical: 10),
            leading: Icon(icon, size: 30),
            title: Text(title, style: const TextStyle(fontSize: 20, fontWeight: FontWeight.w600)),
            trailing: Row(mainAxisSize: MainAxisSize.min, children: [
              if (count != null) Text(count, style: const TextStyle(fontSize: 18, color: Colors.white54)),
              const SizedBox(width: 8),
              const Icon(Icons.chevron_right),
            ]),
            onTap: () => Navigator.of(context).push(MaterialPageRoute(builder: (_) => page())),
          ),
        );

    return ListenableBuilder(
      listenable: state,
      builder: (context, _) => ListView(
        children: [
          const Padding(
            padding: EdgeInsets.all(24),
            child: Row(mainAxisAlignment: MainAxisAlignment.center, children: [
              Icon(Icons.inventory_2, size: 32),
              SizedBox(width: 12),
              Text('Vault', style: TextStyle(fontSize: 30, fontWeight: FontWeight.bold)),
              SizedBox(width: 12),
              Tooltip(message: 'End-to-end encrypted', child: Icon(Icons.lock_outline, color: Colors.white54)),
            ]),
          ),
          tile(Icons.dns, 'Hosts', '${state.core.listHosts().length}', () => HostsScreen(state: state)),
          tile(Icons.key, 'Keychain', '${state.core.listCredentials().length}', () => CredentialsScreen(state: state)),
          tile(Icons.fingerprint, 'Hardware keys', null, () => HardwareKeysScreen(state: state)),
          tile(Icons.data_object, 'Snippets', '${state.core.listSnippets().length}', () => SnippetsScreen(state: state)),
        ],
      ),
    );
  }
}
