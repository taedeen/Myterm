import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';

import 'app_state.dart';

class SyncError implements Exception {
  SyncError(this.message, [this.status]);
  final String message;
  final int? status;
  @override
  String toString() => message;
}

/// Talks to a MyTerm sync server (server/README.md).
///
/// Everything uploaded is encrypted on the device under the vault master key;
/// the server sees opaque keys and ciphertext only. The vault id and access
/// token are derived from the master key, so any device in the vault can
/// sync without extra secrets. The server token is only needed to create a
/// vault or join one, and is not stored.
///
/// A sync is: pull other devices' changes since the last cursor, merge them,
/// then push records whose content changed since they were last sent.
class SyncService extends ChangeNotifier {
  SyncService(this.state);
  final AppState state;

  Timer? _periodic;
  Timer? _debounce;
  bool _running = false;
  /// A sync was asked for while one was running: run again after it, so a
  /// change made mid-sync isn't left waiting for the next minute.
  bool _again = false;
  bool get running => _running;
  String? lastError;

  Map<String, dynamic> get _meta => state.meta;
  bool get configured => _meta['syncUrl'] != null;
  String? get url => _meta['syncUrl'] as String?;
  String? get vaultName => _meta['syncVaultName'] as String?;
  DateTime? get lastSync {
    final t = _meta['syncLastAt'] as int?;
    return t == null ? null : DateTime.fromMillisecondsSinceEpoch(t);
  }

  /// Default server: the MyTerm sync server behind the home reverse proxy.
  static const defaultUrl = 'https://myterm.example.com';

  static String normaliseUrl(String raw) {
    var u = raw.trim();
    if (u.isEmpty) u = defaultUrl;
    if (!u.contains('://')) u = 'https://$u';
    while (u.endsWith('/')) {
      u = u.substring(0, u.length - 1);
    }
    final parsed = Uri.tryParse(u);
    if (parsed == null || !(parsed.scheme == 'http' || parsed.scheme == 'https') || parsed.host.isEmpty) {
      throw SyncError('Enter a server address like $defaultUrl');
    }
    return u;
  }

  // ------------------------------------------------------------- HTTP

  Future<Map<String, dynamic>> _http(
    String base,
    String method,
    String path, {
    Object? body,
    String? serverToken,
    String? vaultToken,
  }) async {
    final client = HttpClient()..connectionTimeout = const Duration(seconds: 10);
    try {
      final req = await client.openUrl(method, Uri.parse('$base$path'));
      req.headers.set(HttpHeaders.acceptHeader, 'application/json');
      if (serverToken != null) req.headers.set(HttpHeaders.authorizationHeader, 'Bearer $serverToken');
      if (vaultToken != null) req.headers.set('X-MyTerm-Vault-Token', vaultToken);
      if (body != null) {
        final bytes = utf8.encode(jsonEncode(body));
        req.headers.contentType = ContentType.json;
        req.contentLength = bytes.length;
        req.add(bytes);
      }
      final res = await req.close().timeout(const Duration(seconds: 60));
      final text = await res.transform(utf8.decoder).join();
      Map<String, dynamic> data = {};
      if (text.isNotEmpty) {
        try {
          data = jsonDecode(text) as Map<String, dynamic>;
        } catch (_) {
          throw SyncError('Not a MyTerm sync server (HTTP ${res.statusCode})', res.statusCode);
        }
      }
      if (res.statusCode >= 300) {
        throw SyncError(data['error'] as String? ?? 'HTTP ${res.statusCode}', res.statusCode);
      }
      return data;
    } on SocketException catch (e) {
      throw SyncError("Can't reach the sync server: ${e.osError?.message ?? e.message}");
    } on HandshakeException {
      throw SyncError("TLS handshake failed: the server's certificate isn't trusted by this device. Check the proxy's certificate.");
    } on TimeoutException {
      throw SyncError('The sync server took too long to answer.');
    } finally {
      client.close(force: true);
    }
  }

  // ------------------------------------------------------------- setup

  /// First device: registers this vault on the server.
  Future<void> createVault({required String url, required String serverToken, required String name}) async {
    final base = normaliseUrl(url);
    await _http(base, 'GET', '/v1/health');
    final id = state.core.syncIdentity();
    await _http(base, 'POST', '/v1/vaults', serverToken: serverToken.trim(), body: {
      'name': name.trim().toLowerCase(),
      'vault_id': id.vaultId,
      'auth_hash': id.authHash,
      'wrapped': state.wrappedForServer(),
    });
    await _saveConfig(base, name);
    await syncNow();
  }

  /// Another device: joins the vault registered under [name]. This device's
  /// own hosts and credentials are kept and merged in.
  Future<void> joinVault({
    required String url,
    required String serverToken,
    required String name,
    required String masterPassword,
  }) async {
    final base = normaliseUrl(url);
    final found = await _http(
      base,
      'GET',
      '/v1/vaults/lookup?name=${Uri.encodeQueryComponent(name.trim().toLowerCase())}',
      serverToken: serverToken.trim(),
    );
    final w = found['wrapped'] as Map<String, dynamic>;
    final Uint8List mk;
    try {
      mk = await state.core.joinVault(
        masterPassword: masterPassword,
        passwordWrapped: base64Decode(w['password_wrapped'] as String),
        kdfSalt: base64Decode(w['kdf_salt'] as String),
        kdfMKib: w['kdf_m_kib'] as int,
        kdfT: w['kdf_t'] as int,
        kdfP: w['kdf_p'] as int,
        expectedVaultId: found['vault_id'] as String,
      );
    } catch (e) {
      throw SyncError(e.toString().contains('wrong master password')
          ? 'Wrong master password for that vault.'
          : 'Could not join: $e');
    }
    await state.adoptJoinedVault(mk, w);
    _meta.remove('syncDigests'); // everything is re-encrypted: upload it all
    await _saveConfig(base, name);
    await syncNow();
  }

  Future<void> _saveConfig(String base, String name) async {
    _meta
      ..['syncUrl'] = base
      ..['syncVaultName'] = name.trim().toLowerCase()
      ..['syncCursor'] = 0;
    await state.writeMeta();
    start();
    notifyListeners();
  }

  /// Stops syncing on this device. Data stays on the device and the server.
  Future<void> disconnect() async {
    stop();
    for (final k in ['syncUrl', 'syncVaultName', 'syncCursor', 'syncDigests', 'syncLastAt']) {
      _meta.remove(k);
    }
    lastError = null;
    await state.writeMeta();
    notifyListeners();
  }

  // ------------------------------------------------------------- running

  /// Periodic sync while unlocked.
  void start() {
    if (!configured) return;
    _periodic ??= Timer.periodic(const Duration(minutes: 1), (_) => syncNow());
  }

  void stop() {
    _periodic?.cancel();
    _periodic = null;
    _debounce?.cancel();
  }

  /// Sync a few seconds after the last local change.
  void soon() {
    if (!configured) return;
    _debounce?.cancel();
    _debounce = Timer(const Duration(seconds: 5), syncNow);
  }

  /// Pushes pending changes now (before locking or going to the background),
  /// waiting at most [limit].
  Future<void> flush({Duration limit = const Duration(seconds: 15)}) async {
    if (!configured || !state.unlocked) return;
    _debounce?.cancel();
    try {
      await syncNow().timeout(limit);
    } on TimeoutException {
      // Best effort: the next unlock syncs whatever is still pending.
    }
  }

  Future<void> syncNow() async {
    final base = url;
    if (base == null || !state.unlocked) return;
    if (_running) {
      _again = true;
      return;
    }
    _running = true;
    _again = false;
    notifyListeners();
    try {
      final id = state.core.syncIdentity();
      final vault = '/v1/vaults/${id.vaultId}/records';
      final me = state.deviceId;

      // 1. Pull other devices' changes.
      var cursor = (_meta['syncCursor'] as int?) ?? 0;
      var changed = 0;
      while (true) {
        final page = await _http(base, 'GET', '$vault?since=$cursor', vaultToken: id.vaultToken);
        final blobs = [
          for (final r in (page['records'] as List).cast<Map<String, dynamic>>())
            if (r['device_id'] != me) base64Decode(r['data'] as String),
        ];
        if (blobs.isNotEmpty) changed += await state.core.syncApply(records: blobs);
        cursor = page['cursor'] as int;
        if (page['more'] != true) break;
      }
      _meta['syncCursor'] = cursor;

      // 2. Push what changed here (including merges just made).
      final items = await state.core.syncCollect();
      final sent = Map<String, String>.from((_meta['syncDigests'] as Map?) ?? const {});
      final todo = items.where((i) => sent[i.key] != i.digest).toList();
      for (var i = 0; i < todo.length; i += 200) {
        final batch = todo.sublist(i, i + 200 > todo.length ? todo.length : i + 200);
        await _http(base, 'POST', vault, vaultToken: id.vaultToken, body: {
          'device_id': me,
          'records': [
            for (final it in batch) {'key': it.key, 'data': base64Encode(it.data)},
          ],
        });
        for (final it in batch) {
          sent[it.key] = it.digest;
        }
        _meta['syncDigests'] = sent;
      }

      _meta['syncLastAt'] = DateTime.now().millisecondsSinceEpoch;
      lastError = null;
      await state.writeMeta();
      if (changed > 0) {
        await state.save(syncAfter: false);
        state.refresh();
      }
    } on SyncError catch (e) {
      lastError = e.status == 401
          ? 'The server rejected this vault. It may have been deleted or reset on the server.'
          : e.message;
    } catch (e) {
      lastError = '$e';
    } finally {
      _running = false;
      notifyListeners();
    }
    if (_again && state.unlocked) {
      _again = false;
      await syncNow();
    }
  }

  @override
  void dispose() {
    stop();
    super.dispose();
  }
}
