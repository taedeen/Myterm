import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';

import 'app_state.dart';
import 'rust/api.dart' show CertificateDto;

class SyncError implements Exception {
  SyncError(this.message, [this.status, this.body = const {}]);
  final String message;
  final int? status;
  final Map<String, dynamic> body;
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
  /// What the last sync did, e.g. "Received 3 changes, sent 1".
  String? lastResult;

  /// Another device revoked a device and moved the vault to a new key; this
  /// device needs the master password to follow (see [followMove]).
  bool get moved => _meta['syncMoved'] == true;

  Map<String, dynamic> get _meta => state.meta;
  bool get configured => _meta['syncUrl'] != null;
  String? get url => _meta['syncUrl'] as String?;
  String? get vaultName => _meta['syncVaultName'] as String?;
  DateTime? get lastSync {
    final t = _meta['syncLastAt'] as int?;
    return t == null ? null : DateTime.fromMillisecondsSinceEpoch(t);
  }

  /// Default server: the MyTerm sync server behind the home reverse proxy.
  static const defaultUrl = 'https://myterm.local.taedeen.com';

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
      if (vaultToken != null) {
        req.headers.set('X-MyTerm-Vault-Token', vaultToken);
        // For the vault's device list. Header values must be plain ASCII.
        req.headers.set('X-MyTerm-Device', state.deviceId);
        final name = state.deviceName.replaceAll(RegExp(r'[^\x20-\x7E]'), '').trim();
        if (name.isNotEmpty) req.headers.set('X-MyTerm-Device-Name', name.length > 64 ? name.substring(0, 64) : name);
      }
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
        throw SyncError(data['error'] as String? ?? 'HTTP ${res.statusCode}', res.statusCode, data);
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
    for (final k in ['syncUrl', 'syncVaultName', 'syncCursor', 'syncDigests', 'syncLastAt', 'syncMoved']) {
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
    if (base == null || !state.unlocked || moved) return;
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
      var received = 0;
      var resetDetected = false;
      while (true) {
        final page = await _http(base, 'GET', '$vault?since=$cursor', vaultToken: id.vaultToken);
        if ((page['cursor'] as int) < cursor) {
          // The server is behind what this device has seen: its database was
          // reset or restored. Start over, and send everything again since
          // what it held before is gone.
          cursor = 0;
          _meta.remove('syncDigests');
          resetDetected = true;
          continue;
        }
        final blobs = [
          for (final r in (page['records'] as List).cast<Map<String, dynamic>>())
            if (r['device_id'] != me) base64Decode(r['data'] as String),
        ];
        received += blobs.length;
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
      if (state.core.sshCertificate() != null) unawaited(renewCertificate());
      lastResult = 'Received $received ($changed changed here), sent ${todo.length}';
      // Log syncs that did something; quiet ones every minute would bury the rest.
      if (changed > 0 || todo.isNotEmpty || resetDetected) {
        state.core.auditSync(detail: '${resetDetected ? 'server was reset; resent everything. ' : ''}$lastResult');
      }
      await state.writeMeta();
      if (changed > 0) {
        await state.save(syncAfter: false);
        state.refresh();
      }
    } on SyncError catch (e) {
      if (e.status == 410 && e.body['moved'] == true) {
        _meta['syncMoved'] = true;
        await state.writeMeta();
        _failed('A device was revoked on another device, so your vault has a new key. '
            'Enter your master password to keep syncing.');
        return;
      }
      _failed(e.status == 401
          ? 'The server rejected this vault. It may have been deleted or reset on the server: '
              'Disconnect, then set up sync again.'
          : e.message);
    } catch (e) {
      _failed('$e');
    } finally {
      _running = false;
      notifyListeners();
    }
    if (_again && state.unlocked) {
      _again = false;
      await syncNow();
    }
  }

  void _failed(String message) {
    // Log each new error once, not every minute while it persists.
    if (message != lastError && state.unlocked) {
      try {
        state.core.auditSync(detail: 'failed: $message');
      } catch (_) {}
    }
    lastError = message;
    lastResult = null;
  }

  // ------------------------------------------------------------- SSH certificates

  /// Last certificate problem (e.g. not enabled on the server), for Settings.
  String? certError;
  Future<CertificateDto?>? _renewing;

  /// Gets a new certificate from the server when this device has none or it
  /// expires within [margin]. Returns the current certificate.
  Future<CertificateDto?> renewCertificate({bool force = false, Duration margin = const Duration(hours: 2)}) {
    return _renewing ??= _renew(force, margin).whenComplete(() => _renewing = null);
  }

  Future<CertificateDto?> _renew(bool force, Duration margin) async {
    final current = state.core.sshCertificate();
    final left = current == null
        ? Duration.zero
        : DateTime.fromMillisecondsSinceEpoch(current.validBefore.toInt() * 1000).difference(DateTime.now());
    if (!force && left > margin) return current;
    if (!configured || moved || !state.unlocked) return current;
    try {
      final pub = await state.core.certificatePublicKey(comment: 'myterm ${state.deviceName}');
      final id = state.core.syncIdentity();
      final r = await _http(url!, 'POST', '/v1/vaults/${id.vaultId}/ssh-cert',
          vaultToken: id.vaultToken, body: {'public_key': pub});
      final cert = await state.core.setSshCertificate(certificate: r['certificate'] as String);
      certError = null;
      await state.save(syncAfter: false);
      notifyListeners();
      return cert;
    } on SyncError catch (e) {
      certError = e.status == 404
          ? 'SSH certificates aren\'t turned on on the sync server (MYTERM_SYNC_CERT_PRINCIPALS).'
          : e.message;
      notifyListeners();
      return current;
    }
  }

  /// The CA public key servers trust, from the sync server.
  Future<String> caPublicKey() async {
    final client = HttpClient()..connectionTimeout = const Duration(seconds: 10);
    try {
      final req = await client.getUrl(Uri.parse('${url!}/v1/ssh-ca.pub'));
      final res = await req.close().timeout(const Duration(seconds: 20));
      final text = await res.transform(utf8.decoder).join();
      if (res.statusCode != 200) {
        throw SyncError('SSH certificates aren\'t turned on on the sync server.', res.statusCode);
      }
      return text.trim();
    } on SocketException catch (e) {
      throw SyncError("Can't reach the sync server: ${e.osError?.message ?? e.message}");
    } finally {
      client.close(force: true);
    }
  }

  // ------------------------------------------------------------- devices

  /// Devices that have synced with this vault, most recently seen first.
  Future<List<SyncDevice>> devices() async {
    final id = state.core.syncIdentity();
    final r = await _http(url!, 'GET', '/v1/vaults/${id.vaultId}/devices', vaultToken: id.vaultToken);
    return [for (final d in (r['devices'] as List).cast<Map<String, dynamic>>()) SyncDevice.fromJson(d)];
  }

  /// A retired device: removes its copies from the server. Your other
  /// devices keep everything they already merged.
  Future<void> forgetDevice(String deviceId) async {
    final id = state.core.syncIdentity();
    await _http(url!, 'DELETE', '/v1/vaults/${id.vaultId}/devices/$deviceId', vaultToken: id.vaultToken);
  }

  /// A lost or stolen device: makes a new master key (with [newPassword] if
  /// given), moves the server's vault to it and re-encrypts this device.
  /// Other devices follow with the master password; the lost one can't.
  Future<void> revokeAndRekey({required String masterPassword, String? newPassword}) async {
    final base = url!;
    stop();
    try {
      final old = state.core.syncIdentity();
      final m = _meta;
      final rot = await state.core.prepareKeyRotation(
        masterPassword: masterPassword,
        newPassword: newPassword,
        passwordWrapped: base64Decode(m['passwordWrapped'] as String),
        kdfSalt: base64Decode(m['salt'] as String),
        kdfMKib: m['m'] as int,
        kdfT: m['t'] as int,
        kdfP: m['p'] as int,
      );
      final wrapped = {
        'password_wrapped': base64Encode(rot.passwordWrapped),
        'kdf_salt': base64Encode(rot.kdfSalt),
        'kdf_m_kib': rot.kdfMKib,
        'kdf_t': rot.kdfT,
        'kdf_p': rot.kdfP,
      };
      // Server first: if this fails nothing has changed here. If the app
      // stops after it, this device follows the move like any other.
      await _http(base, 'POST', '/v1/vaults/${old.vaultId}/successor', vaultToken: old.vaultToken, body: {
        'vault_id': rot.vaultId,
        'auth_hash': rot.authHash,
        'wrapped': wrapped,
      });
      await state.core.applyKeyRotation(masterKey: rot.masterKey);
      await state.adoptJoinedVault(rot.masterKey, wrapped); // wipes the key
      _meta
        ..['syncCursor'] = 0
        ..remove('syncDigests')
        ..remove('syncMoved');
      await state.writeMeta();
    } finally {
      start();
    }
    await syncNow();
  }

  /// After another device revoked one: joins the vault's successor with the
  /// master password (the current one, or the new one if it was changed).
  Future<void> followMove(String masterPassword) async {
    final base = url!;
    final old = state.core.syncIdentity();
    final found = await _http(base, 'GET', '/v1/vaults/${old.vaultId}/successor', vaultToken: old.vaultToken);
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
          ? 'Wrong master password. If it was changed when the device was revoked, use the new one.'
          : 'Could not continue: $e');
    }
    await state.adoptJoinedVault(mk, w);
    _meta
      ..['syncCursor'] = 0
      ..remove('syncDigests')
      ..remove('syncMoved')
      ..['syncVaultName'] = found['name'] as String;
    lastError = null;
    await state.writeMeta();
    start();
    notifyListeners();
    await syncNow();
  }

  /// Forgets what was sent and received, then syncs everything again.
  Future<void> resyncAll() async {
    _meta
      ..['syncCursor'] = 0
      ..remove('syncDigests');
    await state.writeMeta();
    await syncNow();
  }

  @override
  void dispose() {
    stop();
    super.dispose();
  }
}

/// One entry in the vault's device list.
class SyncDevice {
  SyncDevice({required this.id, required this.name, required this.firstSeen, required this.lastSeen, required this.records});
  factory SyncDevice.fromJson(Map<String, dynamic> j) => SyncDevice(
        id: j['device_id'] as String,
        name: j['name'] as String,
        firstSeen: DateTime.fromMillisecondsSinceEpoch((j['first_seen'] as int) * 1000),
        lastSeen: DateTime.fromMillisecondsSinceEpoch((j['last_seen'] as int) * 1000),
        records: j['records'] as int,
      );
  final String id;
  final String name;
  final DateTime firstSeen;
  final DateTime lastSeen;
  final int records;
}
