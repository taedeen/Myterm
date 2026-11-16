import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter/widgets.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart' show ExternalLibrary;
import 'package:path_provider/path_provider.dart';
import 'package:uuid/uuid.dart';

import 'native.dart';
import 'rust/api.dart';
import 'rust/frb_generated.dart';
import 'sync_service.dart';
import 'terminal_session.dart';
import 'terminal_style.dart';

/// Owns the Rust core, the on-disk store and vault unlock state.
///
/// Files (app-private storage):
/// - `meta.json`: device id, password-wrapped and device-wrapped master key,
///   Argon2id parameters. Contains no plaintext secrets.
/// - `store.bin`: vault records (ciphertext only).
class AppState extends ChangeNotifier {
  late final MyTerm core;
  late final Directory _dir;
  Map<String, dynamic> _meta = {};
  /// Open terminal sessions (connecting, live, or ended but not yet closed).
  /// They belong to the app, not to a screen, so they survive navigation.
  final List<TerminalSession> sessions = [];

  /// App-wide navigator, for dialogs raised by sessions in the background.
  static final navigatorKey = GlobalKey<NavigatorState>();

  int get liveCount => sessions.where((s) => s.isLive).length;
  /// Default key bar (Settings), kept across restarts. Basic unless changed.
  String get keybarProfileId => _meta['keybarProfileId'] as String? ?? 'builtin-basic';
  set keybarProfileId(String id) {
    _meta['keybarProfileId'] = id;
    _writeMeta();
    notifyListeners();
  }

  /// Terminal text size on this device (Settings, the Aa button in the
  /// terminal, or Ctrl + = / - / 0). Kept in meta.json, not synced, because
  /// a phone and a desktop want different sizes.
  double get terminalFontSize =>
      (_meta['terminalFontSize'] as num?)?.toDouble() ?? defaultTerminalFontSize;
  set terminalFontSize(double v) {
    final size = v.clamp(minTerminalFontSize, maxTerminalFontSize).roundToDouble();
    if (size == terminalFontSize) return;
    _meta['terminalFontSize'] = size;
    notifyListeners();
    // Dragging the slider changes it many times a second; write once it settles.
    _fontSizeTimer?.cancel();
    _fontSizeTimer = Timer(const Duration(milliseconds: 600), _writeMeta);
  }

  Timer? _fontSizeTimer;

  /// Tapping an autocomplete suggestion also presses Enter (default on).
  bool get runSuggestionsImmediately => _meta['runSuggestions'] as bool? ?? true;
  set runSuggestionsImmediately(bool v) {
    _meta['runSuggestions'] = v;
    notifyListeners();
    _writeMeta();
  }

  /// Sync with a MyTerm sync server (Settings → Sync).
  late final SyncService sync = SyncService(this);

  /// This device's name in the sync device list (Settings → Sync → Devices).
  String get deviceName => (_meta['deviceName'] as String?) ?? _defaultDeviceName;
  String _defaultDeviceName = 'This device';
  set deviceName(String v) {
    final name = v.trim();
    if (name.isEmpty) {
      _meta.remove('deviceName');
    } else {
      _meta['deviceName'] = name;
    }
    _writeMeta();
    notifyListeners();
  }

  /// Device-local settings (meta.json). Holds no secrets in the clear.
  Map<String, dynamic> get meta => _meta;
  String get deviceId => _meta['deviceId'] as String;
  Future<void> writeMeta() => _writeMeta();
  void refresh() => notifyListeners();

  /// The password-wrapped master key, as registered with the sync server so
  /// another device can join with the master password.
  Map<String, dynamic> wrappedForServer() => {
        'password_wrapped': _meta['passwordWrapped'],
        'kdf_salt': _meta['salt'],
        'kdf_m_kib': _meta['m'],
        'kdf_t': _meta['t'],
        'kdf_p': _meta['p'],
      };

  /// After joining another device's vault: its master password now unlocks
  /// this device too, and biometric unlock is re-armed with the new key.
  Future<void> adoptJoinedVault(Uint8List mk, Map<String, dynamic> w) async {
    _meta
      ..['passwordWrapped'] = w['password_wrapped']
      ..['salt'] = w['kdf_salt']
      ..['m'] = w['kdf_m_kib']
      ..['t'] = w['kdf_t']
      ..['p'] = w['kdf_p'];
    try {
      final wrapped = await Native.deviceWrap(mk);
      if (wrapped != null) {
        _meta['deviceWrapped'] = base64Encode(wrapped);
      } else {
        _meta.remove('deviceWrapped');
      }
    } catch (_) {
      _meta.remove('deviceWrapped');
    } finally {
      wipe(mk);
    }
    await _writeMeta();
    await save(syncAfter: false);
    notifyListeners();
  }

  bool get hasVault => _meta['passwordWrapped'] != null;
  bool get hasBiometricUnlock => _meta['deviceWrapped'] != null;
  bool get unlocked => core.isUnlocked();

  File get _metaFile => File('${_dir.path}/meta.json');
  File get _storeFile => File('${_dir.path}/store.bin');

  Future<void> init() async {
    await RustLib.init(
      externalLibrary: ExternalLibrary.open(Platform.isWindows ? 'myterm_ffi.dll' : 'libmyterm_ffi.so'),
    );
    _dir = await getApplicationSupportDirectory();
    if (await _metaFile.exists()) {
      _meta = jsonDecode(await _metaFile.readAsString()) as Map<String, dynamic>;
    }
    _meta['deviceId'] ??= const Uuid().v4();
    _defaultDeviceName = await Native.deviceModel();
    core = MyTerm(deviceId: _meta['deviceId'] as String);
    if (await _storeFile.exists()) {
      await core.importStore(store: await _storeFile.readAsBytes());
    }
  }

  Future<void> _metaWrites = Future.value();

  /// One write at a time: two settings changed in quick succession wrote
  /// the same temp file at once, and on Windows the second rename failed
  /// ("being used by another process").
  Future<void> _writeMeta() => _metaWrites = _metaWrites.catchError((_) {}).then((_) => _writeMetaNow());

  Future<void> _writeMetaNow() async {
    final tmp = File('${_metaFile.path}.tmp');
    await tmp.writeAsString(jsonEncode(_meta), flush: true);
    await tmp.rename(_metaFile.path);
  }

  Timer? _saveTimer;

  /// Saves a few seconds after the last change (e.g. after each command), so
  /// history survives the app being killed or updated.
  void saveSoon() {
    _saveTimer?.cancel();
    _saveTimer = Timer(const Duration(seconds: 3), save);
  }

  Future<void> _saves = Future.value();

  /// Saves are queued for the same reason as meta.json writes.
  Future<void> save({bool syncAfter = true}) {
    _saveTimer?.cancel();
    return _saves = _saves.catchError((_) {}).then((_) => _saveNow(syncAfter));
  }

  Future<void> _saveNow(bool syncAfter) async {
    if (!unlocked) return;
    final blobs = await core.exportStore();
    final tmp = File('${_storeFile.path}.tmp');
    await tmp.writeAsBytes(blobs.store, flush: true);
    await tmp.rename(_storeFile.path);
    if (syncAfter) sync.soon();
  }

  void _unlocked() {
    sync.start();
    sync.syncNow(); // pick up other devices' changes straight away
  }

  /// First run. Returns false if biometric unlock could not be set up
  /// (the vault still works with the master password).
  Future<bool> createVault(String masterPassword) async {
    final setup = await core.createVault(masterPassword: masterPassword, mobile: true);
    _meta
      ..['passwordWrapped'] = base64Encode(setup.passwordWrapped)
      ..['salt'] = base64Encode(setup.kdfSalt)
      ..['m'] = setup.kdfMKib
      ..['t'] = setup.kdfT
      ..['p'] = setup.kdfP;
    final mk = setup.masterKeyForDeviceWrap;
    Uint8List? wrapped;
    try {
      wrapped = await Native.deviceWrap(mk);
    } catch (_) {
      wrapped = null;
    } finally {
      wipe(mk);
    }
    if (wrapped != null) _meta['deviceWrapped'] = base64Encode(wrapped);
    await _writeMeta();
    await save();
    notifyListeners();
    _unlocked();
    return wrapped != null;
  }

  Future<bool> unlockWithBiometrics() async {
    final wrapped = _meta['deviceWrapped'] as String?;
    if (wrapped == null) return false;
    final raw = await Native.deviceUnwrap(base64Decode(wrapped));
    if (raw == null) return false;
    // The channel hands back a read-only view; copy it so it can be wiped.
    final mk = Uint8List.fromList(raw);
    try {
      // Unlocking also loads history from the vault (Rust side).
      await core.unlockWithMasterKey(masterKey: mk);
    } finally {
      wipe(mk);
      wipe(raw);
      notifyListeners();
    }
    _unlocked();
    return true;
  }

  Future<void> unlockWithPassword(String masterPassword) async {
    await core.unlockWithPassword(
      masterPassword: masterPassword,
      passwordWrapped: base64Decode(_meta['passwordWrapped'] as String),
      kdfSalt: base64Decode(_meta['salt'] as String),
      kdfMKib: _meta['m'] as int,
      kdfT: _meta['t'] as int,
      kdfP: _meta['p'] as int,
    );
    notifyListeners();
    _unlocked();
  }

  Future<void> lock() async {
    for (final s in List.of(sessions)) {
      closeSession(s);
    }
    await save(syncAfter: false);
    // Push changes made in the last few seconds before the key is gone;
    // previously the pending sync was cancelled here and a host added just
    // before backgrounding the app stayed on this device.
    await sync.flush();
    sync.stop();
    core.lock();
    notifyListeners();
  }

  /// Creates a session for [host] and starts connecting.
  TerminalSession openSession(HostDto host) {
    final s = TerminalSession(state: this, host: host);
    sessions.add(s);
    notifyListeners();
    s.connect();
    return s;
  }

  void sessionStarted(TerminalSession s) {
    Native.keepAlive(liveCount);
    notifyListeners();
  }

  void sessionEnded(TerminalSession s) {
    Native.keepAlive(liveCount);
    save();
    notifyListeners();
  }

  /// Ends a session and removes it from the list.
  void closeSession(TerminalSession s) {
    s.disconnect();
    sessions.remove(s);
    broadcast.remove(s);
    s.release();
    Native.keepAlive(liveCount);
    notifyListeners();
  }

  /// Sessions that receive the same typing (Broadcast in the session bar).
  /// Typing in any of them goes to all; secure password typing never does.
  final Set<TerminalSession> broadcast = {};
  bool get broadcasting => broadcast.length > 1;

  void setBroadcast(Iterable<TerminalSession> group) {
    broadcast
      ..clear()
      ..addAll(group.where((s) => sessions.contains(s)));
    if (broadcast.length < 2) broadcast.clear();
    notifyListeners();
  }

  /// tmux session name for a persistent session: myterm-1, myterm-2, ... per
  /// host, reusing the lowest free number, so tab 1 on the phone and tab 1
  /// on the PC are the same server-side session.
  String tmuxNameFor(TerminalSession s) {
    final taken = sessions
        .where((o) => !identical(o, s) && o.host.id == s.host.id)
        .map((o) => o.tmuxName)
        .whereType<String>()
        .toSet();
    for (var i = 1;; i++) {
      final name = 'myterm-$i';
      if (!taken.contains(name)) return name;
    }
  }

  /// Back in front: sessions waiting to reconnect go now.
  void reconnectWaiting() {
    for (final s in sessions) {
      s.reconnectNow();
    }
  }

  int activeSessionsFor(String hostId) => sessions.where((s) => s.host.id == hostId && s.isLive).length;

  static String newId() => const Uuid().v4();

  /// Copy of [h] with a different saved credential.
  static HostDto withCredential(HostDto h, String? credentialId) => HostDto(
        id: h.id,
        label: h.label,
        address: h.address,
        port: h.port,
        username: h.username,
        groupId: h.groupId,
        credentialId: credentialId,
        hardwareKeyId: h.hardwareKeyId,
        tags: h.tags,
        os: h.os,
        keybarProfileId: h.keybarProfileId,
        hardwareKeysOnly: h.hardwareKeysOnly,
        syncHistory: h.syncHistory,
        jumpChain: h.jumpChain,
        useCertificate: h.useCertificate,
        persistent: h.persistent,
      );

  /// Saves [password] to the Keychain as [host]'s login password and links it
  /// to the host. Updates the host's existing password entry if it has one.
  /// Wipes [password] afterwards. Does not write the host itself unless
  /// [updateHost] is true.
  Future<String> saveHostPassword(HostDto host, Uint8List password, {bool updateHost = true}) async {
    final existing = core
        .listCredentials()
        .where((c) => c.id == host.credentialId && c.kind == CredentialKind.password)
        .firstOrNull;
    final id = existing?.id ?? newId();
    try {
      await core.putCredential(
        cred: CredentialDto(
          id: id,
          name: existing?.name ?? '${host.label.isEmpty ? host.address : host.label} password',
          kind: CredentialKind.password,
          confirm: existing?.confirm ?? ConfirmDto.oncePerSession,
          neverAutoType: existing?.neverAutoType ?? false,
          appendEnter: true,
          deviceOnly: existing?.deviceOnly ?? false,
        ),
        secret: password,
      );
    } finally {
      wipe(password);
    }
    if (updateHost) await core.upsertHost(host: withCredential(host, id));
    await save();
    return id;
  }
}
