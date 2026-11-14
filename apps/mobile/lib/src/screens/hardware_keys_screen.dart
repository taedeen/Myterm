import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../app_state.dart';
import '../native.dart';

/// Hardware-bound SSH keys in StrongBox / TEE (spec 3.4.1).
class HardwareKeysScreen extends StatefulWidget {
  const HardwareKeysScreen({super.key, required this.state});
  final AppState state;

  @override
  State<HardwareKeysScreen> createState() => _HardwareKeysScreenState();
}

class _HardwareKeysScreenState extends State<HardwareKeysScreen> {
  List<HardwareKey> _keys = [];

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    final k = await Native.listHardwareKeys();
    if (mounted) setState(() => _keys = k);
  }

  Future<void> _create() async {
    final alias = 'phone-${DateTime.now().millisecondsSinceEpoch ~/ 1000}';
    try {
      await Native.generateHardwareKey(alias);
    } catch (e) {
      if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('Could not create key: $e')));
    }
    await _load();
  }

  String _explain(String p) => switch (p) {
        'StrongBox' => 'Stored in a separate security chip (StrongBox). Cannot be copied off this phone.',
        'TEE' => 'Stored in the phone\'s secure processor area (TEE). Cannot be copied off this phone.',
        _ => 'Software-backed: this phone has no secure hardware for keys.',
      };

  void _show(HardwareKey k) {
    final core = widget.state.core;
    final line = core.hardwarePublicKeyLine(publicSec1: k.publicSec1, comment: 'myterm ${k.alias}');
    final fp = core.hardwareKeyFingerprint(publicSec1: k.publicSec1);
    showModalBottomSheet(
      context: context,
      isScrollControlled: true,
      builder: (ctx) => Padding(
        padding: const EdgeInsets.all(20),
        child: Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
          Text(k.alias, style: Theme.of(ctx).textTheme.titleLarge),
          const SizedBox(height: 8),
          Text(_explain(k.protection)),
          const SizedBox(height: 16),
          const Text('Enrol on a host: add this line to ~/.ssh/authorized_keys'),
          const SizedBox(height: 8),
          SelectableText(line, style: const TextStyle(fontFamily: 'monospace', fontSize: 12)),
          const SizedBox(height: 8),
          Text(fp, style: const TextStyle(fontFamily: 'monospace', fontSize: 12, color: Colors.white54)),
          const SizedBox(height: 16),
          const Text(
            'Hardware keys cannot be backed up. Enrol at least two keys (for example this phone and your desktop) '
            'on each host so losing one device does not lock you out.',
            style: TextStyle(color: Colors.amberAccent),
          ),
          const SizedBox(height: 16),
          Row(children: [
            FilledButton.icon(
              onPressed: () {
                Clipboard.setData(ClipboardData(text: line));
                Navigator.pop(ctx);
              },
              icon: const Icon(Icons.copy),
              label: const Text('Copy public key'),
            ),
            const Spacer(),
            TextButton(
              onPressed: () async {
                await Native.deleteHardwareKey(k.alias);
                if (ctx.mounted) Navigator.pop(ctx);
                await _load();
              },
              child: const Text('Delete'),
            ),
          ]),
        ]),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Hardware keys')),
      floatingActionButton: FloatingActionButton.extended(
        onPressed: _create,
        icon: const Icon(Icons.add),
        label: const Text('Create hardware key'),
      ),
      body: _keys.isEmpty
          ? const Padding(
              padding: EdgeInsets.all(32),
              child: Text(
                'A hardware key lives in this phone\'s security chip. Every login asks for your fingerprint, '
                'face or PIN, and the private key can never be copied or synced.',
                textAlign: TextAlign.center,
              ),
            )
          : ListView(children: [
              for (final k in _keys)
                Card(
                  child: ListTile(
                    leading: const Icon(Icons.fingerprint),
                    title: Text(k.alias),
                    subtitle: Text('ecdsa-sha2-nistp256 · ${k.protection}'),
                    trailing: const Icon(Icons.chevron_right),
                    onTap: () => _show(k),
                  ),
                ),
            ]),
    );
  }
}
