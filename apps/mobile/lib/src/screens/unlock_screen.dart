import 'package:flutter/material.dart';

import '../app_state.dart';
import '../native.dart';

/// First-run vault creation, then biometric or master-password unlock.
class UnlockScreen extends StatefulWidget {
  const UnlockScreen({super.key, required this.state});
  final AppState state;

  @override
  State<UnlockScreen> createState() => _UnlockScreenState();
}

class _UnlockScreenState extends State<UnlockScreen> {
  final _pw = TextEditingController();
  final _pw2 = TextEditingController();
  bool _busy = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    Native.setSecureWindow(true);
    if (widget.state.hasBiometricUnlock) {
      WidgetsBinding.instance.addPostFrameCallback((_) => _biometric());
    }
  }

  @override
  void dispose() {
    Native.setSecureWindow(false);
    _pw.dispose();
    _pw2.dispose();
    super.dispose();
  }

  Future<void> _run(Future<void> Function() f) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await f();
    } catch (e) {
      setState(() => _error = e.toString());
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _biometric() => _run(() async {
        if (!await widget.state.unlockWithBiometrics()) {
          throw '${Native.unlockName[0].toUpperCase()}${Native.unlockName.substring(1)} unlock cancelled. Use your master password.';
        }
      });

  Future<void> _create() => _run(() async {
        if (_pw.text.length < 12) throw 'Use at least 12 characters.';
        if (_pw.text != _pw2.text) throw "Passwords don't match.";
        final bio = await widget.state.createVault(_pw.text);
        if (!bio && mounted) {
          ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(content: Text('${Native.unlockName} unlock unavailable; you will unlock with your master password.')),
          );
        }
      });

  Future<void> _unlock() => _run(() async {
        try {
          await widget.state.unlockWithPassword(_pw.text);
        } catch (_) {
          throw 'Wrong master password.';
        }
      });

  @override
  Widget build(BuildContext context) {
    final creating = !widget.state.hasVault;
    return Scaffold(
      body: SafeArea(
        child: ListView(
          padding: const EdgeInsets.all(24),
          children: [
            const SizedBox(height: 48),
            const Icon(Icons.terminal, size: 64),
            const SizedBox(height: 16),
            Text('MyTerm', textAlign: TextAlign.center, style: Theme.of(context).textTheme.headlineMedium),
            const SizedBox(height: 8),
            Text(
              creating
                  ? 'Create a master password. It encrypts your hosts, keys and history, and is needed on new devices. It cannot be recovered.'
                  : 'Your vault is locked.',
              textAlign: TextAlign.center,
            ),
            const SizedBox(height: 32),
            TextField(
              controller: _pw,
              obscureText: true,
              enableSuggestions: false,
              autocorrect: false,
              decoration: const InputDecoration(labelText: 'Master password', border: OutlineInputBorder()),
              onSubmitted: (_) => creating ? null : _unlock(),
            ),
            if (creating) ...[
              const SizedBox(height: 12),
              TextField(
                controller: _pw2,
                obscureText: true,
                enableSuggestions: false,
                autocorrect: false,
                decoration: const InputDecoration(labelText: 'Repeat master password', border: OutlineInputBorder()),
              ),
            ],
            if (_error != null) ...[
              const SizedBox(height: 12),
              Text(_error!, style: TextStyle(color: Theme.of(context).colorScheme.error)),
            ],
            const SizedBox(height: 24),
            FilledButton(
              onPressed: _busy ? null : (creating ? _create : _unlock),
              child: Text(_busy ? 'Working…' : (creating ? 'Create vault' : 'Unlock')),
            ),
            if (!creating && widget.state.hasBiometricUnlock) ...[
              const SizedBox(height: 12),
              OutlinedButton.icon(
                onPressed: _busy ? null : _biometric,
                icon: const Icon(Icons.fingerprint),
                label: Text('Unlock with ${Native.unlockName}'),
              ),
            ],
          ],
        ),
      ),
    );
  }
}
