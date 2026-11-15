import 'package:flutter/material.dart';

import '../app_state.dart';
import '../sync_service.dart';

/// Settings card: sync status, or the way into setup.
class SyncCard extends StatelessWidget {
  const SyncCard({super.key, required this.state});
  final AppState state;

  String _ago(DateTime t) {
    final d = DateTime.now().difference(t);
    if (d.inSeconds < 60) return 'just now';
    if (d.inMinutes < 60) return '${d.inMinutes} min ago';
    if (d.inHours < 24) return '${d.inHours} h ago';
    return '${t.day}/${t.month} ${t.hour}:${t.minute.toString().padLeft(2, '0')}';
  }

  @override
  Widget build(BuildContext context) {
    final sync = state.sync;
    return ListenableBuilder(
      listenable: sync,
      builder: (context, _) {
        if (!sync.configured) {
          return Card(
            child: ListTile(
              leading: const Icon(Icons.sync_disabled),
              title: const Text('Sync'),
              subtitle: const Text('Keep hosts, keys, snippets and history in step across your devices'),
              trailing: const Icon(Icons.chevron_right),
              onTap: () => Navigator.of(context).push(MaterialPageRoute(builder: (_) => SyncSetupScreen(state: state))),
            ),
          );
        }
        final last = sync.lastSync;
        final insecure = sync.url!.startsWith('http://');
        return Card(
          child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
            ListTile(
              leading: Icon(sync.lastError == null ? Icons.sync : Icons.sync_problem,
                  color: sync.lastError == null ? const Color(0xFF2ECC71) : Colors.orange),
              title: Text('Sync · ${sync.vaultName}'),
              subtitle: Text([
                sync.url!,
                if (sync.running) 'Syncing…' else if (last != null) 'Last synced ${_ago(last)}' else 'Not synced yet',
                if (sync.lastError != null) sync.lastError!,
              ].join('\n')),
              isThreeLine: true,
            ),
            if (insecure)
              const Padding(
                padding: EdgeInsets.fromLTRB(16, 0, 16, 8),
                child: Text(
                  'Plain http: data is end-to-end encrypted, but use https if this server is reachable from outside your home network.',
                  style: TextStyle(fontSize: 12, color: Colors.white54),
                ),
              ),
            OverflowBar(alignment: MainAxisAlignment.end, children: [
              TextButton(
                onPressed: () async {
                  final ok = await showDialog<bool>(
                    context: context,
                    builder: (ctx) => AlertDialog(
                      title: const Text('Stop syncing on this device?'),
                      content: const Text('Your data stays on this device and on the server. You can join again later.'),
                      actions: [
                        TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
                        FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Stop syncing')),
                      ],
                    ),
                  );
                  if (ok == true) await sync.disconnect();
                },
                child: const Text('Stop syncing'),
              ),
              FilledButton.tonal(
                onPressed: sync.running ? null : sync.syncNow,
                child: const Text('Sync now'),
              ),
            ]),
          ]),
        );
      },
    );
  }
}

/// Create a sync vault (first device) or join one (other devices).
class SyncSetupScreen extends StatefulWidget {
  const SyncSetupScreen({super.key, required this.state});
  final AppState state;

  @override
  State<SyncSetupScreen> createState() => _SyncSetupScreenState();
}

class _SyncSetupScreenState extends State<SyncSetupScreen> {
  final _url = TextEditingController(text: SyncService.defaultUrl);
  final _token = TextEditingController();
  final _name = TextEditingController();
  final _password = TextEditingController();
  bool _join = false;
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _password.clear();
    for (final c in [_url, _token, _name, _password]) {
      c.dispose();
    }
    super.dispose();
  }

  Future<void> _go() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final sync = widget.state.sync;
      if (_token.text.trim().isEmpty || _name.text.trim().length < 3) {
        throw 'Fill in the server token and a vault name (3+ characters).';
      }
      if (_join) {
        if (_password.text.isEmpty) throw 'Enter the master password of the vault you are joining.';
        await sync.joinVault(url: _url.text, serverToken: _token.text, name: _name.text, masterPassword: _password.text);
      } else {
        await sync.createVault(url: _url.text, serverToken: _token.text, name: _name.text);
      }
      _password.clear();
      if (mounted) Navigator.pop(context);
    } catch (e) {
      setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    InputDecoration dec(String l, [String? help]) =>
        InputDecoration(labelText: l, helperText: help, helperMaxLines: 3, border: const OutlineInputBorder());
    return Scaffold(
      appBar: AppBar(title: const Text('Set up sync')),
      body: ListView(padding: const EdgeInsets.all(16), children: [
        SegmentedButton<bool>(
          segments: const [
            ButtonSegment(value: false, label: Text('New sync vault'), icon: Icon(Icons.add)),
            ButtonSegment(value: true, label: Text('Join existing'), icon: Icon(Icons.login)),
          ],
          selected: {_join},
          onSelectionChanged: (s) => setState(() => _join = s.first),
        ),
        const SizedBox(height: 12),
        Text(
          _join
              ? 'Use this on your second and later devices. This device then uses that vault\'s master password, '
                  'and its own hosts and credentials are merged in.'
              : 'Use this on your first device. It registers your vault with the server; other devices then join it.',
          style: const TextStyle(color: Colors.white70),
        ),
        const SizedBox(height: 16),
        TextField(
          controller: _url,
          keyboardType: TextInputType.url,
          autocorrect: false,
          decoration: dec('Server address', 'Default ${SyncService.defaultUrl}'),
        ),
        const SizedBox(height: 12),
        TextField(
          controller: _token,
          obscureText: true,
          autocorrect: false,
          enableSuggestions: false,
          decoration: dec('Server token', 'MYTERM_SYNC_TOKEN from the server. Only used for this setup; not stored.'),
        ),
        const SizedBox(height: 12),
        TextField(
          controller: _name,
          autocorrect: false,
          decoration: dec('Vault name', _join ? 'The name chosen on your first device' : 'Any name, e.g. home'),
        ),
        if (_join) ...[
          const SizedBox(height: 12),
          TextField(
            controller: _password,
            obscureText: true,
            autocorrect: false,
            enableSuggestions: false,
            decoration: dec('Master password of that vault'),
          ),
        ],
        if (_error != null) ...[
          const SizedBox(height: 12),
          Text(_error!, style: TextStyle(color: Theme.of(context).colorScheme.error)),
        ],
        const SizedBox(height: 20),
        FilledButton(
          onPressed: _busy ? null : _go,
          child: Text(_busy ? 'Working…' : (_join ? 'Join and sync' : 'Create and sync')),
        ),
      ]),
    );
  }
}
