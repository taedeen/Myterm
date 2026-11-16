import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

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
                if (sync.lastResult != null) sync.lastResult!,
                if (sync.lastError != null) sync.lastError!,
                // Devices sync with each other only if these vault ids match.
                'Vault id ${state.core.syncIdentity().vaultId.substring(0, 8)} · this device ${state.deviceId.substring(0, 8)}',
              ].join('\n')),
              isThreeLine: true,
            ),
            if (sync.moved)
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
                child: FilledButton.icon(
                  icon: const Icon(Icons.key),
                  label: const Text('Enter master password to keep syncing'),
                  onPressed: () => followMoveDialog(context, state),
                ),
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
              TextButton(
                onPressed: sync.moved
                    ? null
                    : () => Navigator.of(context).push(MaterialPageRoute(builder: (_) => DevicesScreen(state: state))),
                child: const Text('Devices'),
              ),
              TextButton(
                onPressed: sync.running ? null : sync.resyncAll,
                child: const Text('Resend everything'),
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

  /// A paste button for a field: fills it from the clipboard (trimmed).
  Widget _pasteInto(TextEditingController c) => IconButton(
        tooltip: 'Paste',
        icon: const Icon(Icons.content_paste),
        onPressed: () async {
          final t = (await Clipboard.getData(Clipboard.kTextPlain))?.text?.trim();
          if (t == null || t.isEmpty) return;
          // Accept a whole .env line too: MYTERM_SYNC_TOKEN=abc… -> abc…
          c.text = t.startsWith('MYTERM_SYNC_TOKEN=') ? t.substring('MYTERM_SYNC_TOKEN='.length) : t;
          setState(() {});
        },
      );

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
          decoration: dec('Server address', 'Default ${SyncService.defaultUrl}').copyWith(suffixIcon: _pasteInto(_url)),
        ),
        const SizedBox(height: 12),
        TextField(
          controller: _token,
          obscureText: true,
          autocorrect: false,
          enableSuggestions: false,
          decoration: dec(
            'Server token',
            'MYTERM_SYNC_TOKEN from the server\'s .env. Copy it from a MyTerm session to server1 '
                '(cat ~/docker_volumes/myterm-sync/server/.env, long-press, Copy), then paste here. '
                'Only used for this setup; not stored.',
          ).copyWith(suffixIcon: _pasteInto(_token)),
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

/// Asks for the master password and follows a moved vault.
Future<void> followMoveDialog(BuildContext context, AppState state) async {
  final c = TextEditingController();
  String? error;
  var busy = false;
  await showDialog<void>(
    context: context,
    barrierDismissible: false,
    builder: (ctx) => StatefulBuilder(
      builder: (ctx, setState) => AlertDialog(
        title: const Text('Your vault has a new key'),
        content: Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
          const Text('A lost device was revoked from another of your devices. Enter the master password to '
              'keep syncing (the new one, if it was changed).'),
          const SizedBox(height: 12),
          TextField(
            controller: c,
            obscureText: true,
            autofocus: true,
            decoration: InputDecoration(labelText: 'Master password', errorText: error),
          ),
        ]),
        actions: [
          TextButton(onPressed: busy ? null : () => Navigator.pop(ctx), child: const Text('Later')),
          FilledButton(
            onPressed: busy
                ? null
                : () async {
                    setState(() {
                      busy = true;
                      error = null;
                    });
                    try {
                      await state.sync.followMove(c.text);
                      if (ctx.mounted) Navigator.pop(ctx);
                    } catch (e) {
                      setState(() {
                        busy = false;
                        error = '$e';
                      });
                    }
                  },
            child: busy
                ? const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2))
                : const Text('Continue'),
          ),
        ],
      ),
    ),
  );
  c.dispose();
}

/// The vault's devices: rename this one, forget retired ones, revoke lost ones.
class DevicesScreen extends StatefulWidget {
  const DevicesScreen({super.key, required this.state});
  final AppState state;

  @override
  State<DevicesScreen> createState() => _DevicesScreenState();
}

class _DevicesScreenState extends State<DevicesScreen> {
  late Future<List<SyncDevice>> _devices = widget.state.sync.devices();
  bool _busy = false;

  void _reload() => setState(() => _devices = widget.state.sync.devices());

  void _toast(String m) {
    if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(m)));
  }

  String _when(DateTime t) {
    final d = DateTime.now().difference(t);
    if (d.inMinutes < 2) return 'just now';
    if (d.inHours < 1) return '${d.inMinutes} min ago';
    if (d.inDays < 1) return '${d.inHours} h ago';
    if (d.inDays < 60) return '${d.inDays} days ago';
    return '${t.day}/${t.month}/${t.year}';
  }

  Future<void> _rename() async {
    final c = TextEditingController(text: widget.state.deviceName);
    final v = await showDialog<String>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Name of this device'),
        content: TextField(controller: c, autofocus: true, maxLength: 64),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(ctx, c.text), child: const Text('Save')),
        ],
      ),
    );
    if (v == null) return;
    widget.state.deviceName = v;
    await widget.state.sync.syncNow(); // sends the new name
    _reload();
  }

  Future<void> _forget(SyncDevice d) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text('Forget ${d.name.isEmpty ? 'this device' : d.name}?'),
        content: const Text('For a device you no longer use. Its copies are removed from the server; your other '
            'devices keep everything they already have.\n\nIf it was lost or stolen, use Revoke instead: '
            'forgetting doesn\'t stop a device that still has your vault key.'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Forget')),
        ],
      ),
    );
    if (ok != true) return;
    await _run(() => widget.state.sync.forgetDevice(d.id), 'Device forgotten');
  }

  Future<void> _revoke(SyncDevice d) async {
    final pw = TextEditingController();
    final newPw = TextEditingController();
    final again = TextEditingController();
    String? error;
    final go = await showDialog<bool>(
      context: context,
      builder: (ctx) => StatefulBuilder(
        builder: (ctx, setState) => AlertDialog(
          title: Text('Revoke ${d.name.isEmpty ? 'this device' : d.name}?'),
          content: SingleChildScrollView(
            child: Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
              const Text('For a lost or stolen device. Your vault gets a new key: everything is re-encrypted here, '
                  'the server moves to the new vault and deletes the old one, and your other devices ask for the '
                  'master password once. The lost device keeps only the old key and can\'t join without the password.'),
              const SizedBox(height: 12),
              TextField(controller: pw, obscureText: true, decoration: const InputDecoration(labelText: 'Master password')),
              const SizedBox(height: 8),
              const Text('If whoever has the device might know your master password, set a new one:',
                  style: TextStyle(fontSize: 12, color: Colors.white70)),
              TextField(
                controller: newPw,
                obscureText: true,
                decoration: const InputDecoration(labelText: 'New master password (optional)'),
              ),
              TextField(
                controller: again,
                obscureText: true,
                decoration: InputDecoration(labelText: 'New password again', errorText: error),
              ),
            ]),
          ),
          actions: [
            TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
            FilledButton(
              style: FilledButton.styleFrom(backgroundColor: Colors.red.shade700),
              onPressed: () {
                if (pw.text.isEmpty) {
                  setState(() => error = 'Enter the master password');
                } else if (newPw.text != again.text) {
                  setState(() => error = "The new passwords don't match");
                } else if (newPw.text.isNotEmpty && newPw.text.length < 12) {
                  setState(() => error = 'Use at least 12 characters');
                } else {
                  Navigator.pop(ctx, true);
                }
              },
              child: const Text('Revoke'),
            ),
          ],
        ),
      ),
    );
    if (go == true) {
      await _run(
        () async {
          // Its copies go first (still possible with the old key), then the key changes.
          try {
            await widget.state.sync.forgetDevice(d.id);
          } catch (_) {}
          await widget.state.sync.revokeAndRekey(
            masterPassword: pw.text,
            newPassword: newPw.text.isEmpty ? null : newPw.text,
          );
        },
        newPw.text.isEmpty
            ? 'Revoked. Your other devices will ask for the master password once.'
            : 'Revoked. Use the new master password from now on, here and on your other devices.',
      );
    }
    for (final c in [pw, newPw, again]) {
      c.dispose();
    }
  }

  Future<void> _run(Future<void> Function() action, String done) async {
    setState(() => _busy = true);
    try {
      await action();
      _toast(done);
    } catch (e) {
      _toast('$e');
    } finally {
      if (mounted) {
        setState(() => _busy = false);
        _reload();
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final me = widget.state.deviceId;
    return Scaffold(
      appBar: AppBar(title: const Text('Devices'), actions: [
        IconButton(tooltip: 'Refresh', onPressed: _busy ? null : _reload, icon: const Icon(Icons.refresh)),
      ]),
      body: Column(children: [
        if (_busy) const LinearProgressIndicator(),
        Expanded(
          child: FutureBuilder<List<SyncDevice>>(
            future: _devices,
            builder: (context, snap) {
              if (snap.hasError) {
                return Center(child: Padding(padding: const EdgeInsets.all(24), child: Text('${snap.error}')));
              }
              if (!snap.hasData) return const Center(child: CircularProgressIndicator());
              final list = snap.data!;
              if (list.every((d) => d.id != me)) {
                list.insert(0, SyncDevice(id: me, name: widget.state.deviceName, firstSeen: DateTime.now(),
                    lastSeen: DateTime.now(), records: 0));
              }
              return ListView(children: [
                for (final d in list)
                  Card(
                    child: ListTile(
                      leading: Icon(d.id == me ? Icons.smartphone : Icons.devices_other,
                          color: d.id == me ? const Color(0xFF2ECC71) : null),
                      title: Text(d.id == me
                          ? '${widget.state.deviceName} (this device)'
                          : (d.name.isEmpty ? 'Unnamed device ${d.id.substring(0, 8)}' : d.name)),
                      subtitle: Text('Last synced ${_when(d.lastSeen)} · ${d.records} records · id ${d.id.substring(0, 8)}'),
                      trailing: d.id == me
                          ? IconButton(tooltip: 'Rename', onPressed: _busy ? null : _rename, icon: const Icon(Icons.edit))
                          : PopupMenuButton<String>(
                              enabled: !_busy,
                              onSelected: (v) => v == 'forget' ? _forget(d) : _revoke(d),
                              itemBuilder: (_) => const [
                                PopupMenuItem(value: 'forget', child: Text('Forget (no longer used)')),
                                PopupMenuItem(value: 'revoke', child: Text('Revoke (lost or stolen)')),
                              ],
                            ),
                    ),
                  ),
                const Padding(
                  padding: EdgeInsets.all(16),
                  child: Text(
                    'Devices appear after they sync. Forget removes a retired device\'s copies from the server. '
                    'Revoke gives your vault a new key so a lost device can\'t keep syncing.',
                    style: TextStyle(fontSize: 12, color: Colors.white54),
                  ),
                ),
              ]);
            },
          ),
        ),
      ]),
    );
  }
}

/// Settings card: this device's SSH certificate from the sync server's CA.
class CertificateCard extends StatelessWidget {
  const CertificateCard({super.key, required this.state});
  final AppState state;

  String _until(BigInt unix) {
    final t = DateTime.fromMillisecondsSinceEpoch(unix.toInt() * 1000);
    final left = t.difference(DateTime.now());
    final hm = '${t.hour.toString().padLeft(2, '0')}:${t.minute.toString().padLeft(2, '0')}';
    if (left.isNegative) return 'expired at $hm';
    return 'valid until $hm (${left.inHours} h ${left.inMinutes % 60} min)';
  }

  Future<void> _setup(BuildContext context) async {
    String text;
    try {
      final ca = await state.sync.caPublicKey();
      final url = state.sync.url;
      text = '# On each server that should accept MyTerm certificates:\n'
          'curl -fsS $url/v1/ssh-ca.pub | sudo tee /etc/ssh/myterm_user_ca.pub\n'
          'echo "TrustedUserCAKeys /etc/ssh/myterm_user_ca.pub" | sudo tee /etc/ssh/sshd_config.d/myterm-ca.conf\n'
          'sudo sshd -t && (sudo systemctl reload ssh 2>/dev/null || sudo systemctl reload sshd)\n'
          '# (No /etc/ssh/sshd_config.d? Add the TrustedUserCAKeys line to /etc/ssh/sshd_config instead.)\n'
          '\n# The CA key (for reference):\n$ca';
    } catch (e) {
      text = '$e';
    }
    if (!context.mounted) return;
    await showDialog<void>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Trust MyTerm certificates on a server'),
        content: SingleChildScrollView(
          child: Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
            const Text('Run these on the server (or make them a snippet). Then turn on "Sign in with MyTerm '
                'certificate" for that host. Login names must be ones the sync server signs for.'),
            const SizedBox(height: 12),
            SelectableText(text, style: const TextStyle(fontFamily: 'monospace', fontSize: 12)),
          ]),
        ),
        actions: [
          TextButton(
            onPressed: () => Clipboard.setData(ClipboardData(text: text)),
            child: const Text('Copy'),
          ),
          FilledButton(onPressed: () => Navigator.pop(ctx), child: const Text('Done')),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final sync = state.sync;
    return ListenableBuilder(
      listenable: sync,
      builder: (context, _) {
        if (!sync.configured) return const SizedBox.shrink();
        final cert = state.core.sshCertificate();
        return Card(
          child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
            ListTile(
              leading: const Icon(Icons.verified_user),
              title: const Text('SSH certificate'),
              subtitle: Text([
                if (cert == null)
                  'None yet. Short-lived certificates from your sync server sign you in to servers that trust it.'
                else ...[
                  _until(cert.validBefore),
                  'Log in as: ${cert.principals.join(', ')}',
                ],
                if (sync.certError != null) sync.certError!,
              ].join('\n')),
            ),
            OverflowBar(alignment: MainAxisAlignment.end, children: [
              TextButton(onPressed: () => _setup(context), child: const Text('Server setup')),
              FilledButton.tonal(
                onPressed: () => sync.renewCertificate(force: true),
                child: Text(cert == null ? 'Get certificate' : 'Renew now'),
              ),
            ]),
          ]),
        );
      },
    );
  }
}
