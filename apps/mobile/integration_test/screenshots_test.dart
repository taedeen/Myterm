// Screenshots for the user guide, taken from the real app with a demo vault.
//
// Runs in CI (.github/workflows/screenshots.yml) on an Android emulator and
// on Windows, against a throwaway SSH server and sync server set up there:
//
//   flutter drive --driver=test_driver/integration_test.dart \
//     --target=integration_test/screenshots_test.dart \
//     --dart-define=DEMO_SSH_HOST=... --dart-define=DEMO_SYNC_URL=... \
//     --dart-define=DEMO_SYNC_TOKEN=... --dart-define=DEMO_PASSWORD=...
//
// Each screen is drawn into a PNG from a RepaintBoundary around the app and
// handed to the driver (test_driver/integration_test.dart), which writes
// screenshots/<platform>-<name>.png. Nothing personal: example hosts,
// example names, and the sync address shown as https://myterm.example.com.
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:myterm/main.dart';
import 'package:myterm/src/app_state.dart';
import 'package:myterm/src/native.dart';
import 'package:myterm/src/rust/api.dart';
import 'package:myterm/src/screens/hosts_screen.dart';
import 'package:myterm/src/screens/sync_screen.dart' show DevicesScreen;
import 'package:myterm/src/screens/terminal_screen.dart';
import 'package:myterm/src/terminal_session.dart';

const sshHost = String.fromEnvironment('DEMO_SSH_HOST', defaultValue: '127.0.0.1');
const syncUrl = String.fromEnvironment('DEMO_SYNC_URL', defaultValue: 'http://127.0.0.1:7341');
const syncToken = String.fromEnvironment('DEMO_SYNC_TOKEN');
const demoPassword = String.fromEnvironment('DEMO_PASSWORD');
const shownSyncUrl = 'https://myterm.example.com';

final shotKey = GlobalKey();
final shots = <String, String>{};
String get platform => Platform.isAndroid ? 'phone' : 'windows';
bool get phone => Platform.isAndroid;

/// Lets real time pass while drawing frames (network, SSH, timers).
Future<void> settle(WidgetTester t, [int ms = 1200]) async {
  final end = DateTime.now().add(Duration(milliseconds: ms));
  while (DateTime.now().isBefore(end)) {
    await t.pump(const Duration(milliseconds: 100));
  }
}

Future<void> waitFor(WidgetTester t, Finder f, {int seconds = 30}) async {
  final end = DateTime.now().add(Duration(seconds: seconds));
  while (f.evaluate().isEmpty) {
    if (DateTime.now().isAfter(end)) throw TestFailure('Timed out waiting for $f');
    await t.pump(const Duration(milliseconds: 200));
  }
}

Future<void> until(WidgetTester t, bool Function() ok, String Function() what, {int seconds = 30}) async {
  final end = DateTime.now().add(Duration(seconds: seconds));
  while (!ok()) {
    if (DateTime.now().isAfter(end)) throw TestFailure('Timed out waiting for ${what()}');
    await t.pump(const Duration(milliseconds: 200));
  }
}

Future<void> shot(WidgetTester t, String name) async {
  // No on-screen keyboard in the picture (it isn't drawn by Flutter anyway).
  await SystemChannels.textInput.invokeMethod<void>('TextInput.hide');
  await settle(t, 900);
  final boundary = shotKey.currentContext!.findRenderObject()! as RenderRepaintBoundary;
  final image = await boundary.toImage(pixelRatio: t.view.devicePixelRatio);
  final png = await image.toByteData(format: ui.ImageByteFormat.png);
  shots['$platform-$name'] = base64Encode(png!.buffer.asUint8List());
  // ignore: avoid_print
  print('SHOT $platform-$name ${image.width}x${image.height}');
}

NavigatorState get nav => AppState.navigatorKey.currentState!;

Future<void> push(WidgetTester t, Widget page) async {
  nav.push(MaterialPageRoute(builder: (_) => page));
  await settle(t);
}

Future<void> home(WidgetTester t) async {
  nav.popUntil((r) => r.isFirst);
  await settle(t, 600);
}

void type(TerminalSession s, String text) => s.send(Uint8List.fromList(utf8.encode(text)));

Future<void> run(WidgetTester t, TerminalSession s, String command) async {
  type(s, '$command\r');
  await settle(t, 900);
}

HostDto host(String label, String address, String user,
        {List<String> tags = const [], String? os, List<String> via = const [], bool cert = false, bool keep = false}) =>
    HostDto(
      id: AppState.newId(),
      label: label,
      address: address,
      port: 22,
      username: user,
      groupId: null,
      credentialId: null,
      hardwareKeyId: null,
      keybarProfileId: null,
      hardwareKeysOnly: null,
      syncHistory: null,
      tags: tags,
      os: os,
      jumpChain: via,
      useCertificate: cert,
      persistent: keep,
    );

/// Connects to [h] (stored password), trusts its host key, and returns the
/// live session shown on screen.
Future<TerminalSession> connect(WidgetTester t, AppState state, HostDto h, {String? hostKeyShot}) async {
  final s = state.openSession(h);
  nav.push(MaterialPageRoute(builder: (_) => TerminalScreen(state: state, session: s)));
  await waitFor(t, find.text('Trust'));
  if (hostKeyShot != null) await shot(t, hostKeyShot);
  await t.tap(find.text('Trust'));
  // Windows creates the user's profile on the first login, which is slow.
  await until(t, () => s.isLive, () => 'session to ${h.label} (status: ${s.status})', seconds: 90);
  await settle(t, 1500);
  return s;
}

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('user guide screenshots', (t) async {
    Native.skipPlatformAuth = true;
    if (!phone) {
      t.view.physicalSize = const Size(1280, 800);
      t.view.devicePixelRatio = 1.0;
    }
    // Hand over whatever was taken, even if a later step fails.
    addTearDown(() => binding.reportData = {'shots': shots});
    final state = AppState();
    await state.init();
    await t.pumpWidget(RepaintBoundary(key: shotKey, child: MyTermApp(state: state)));
    await settle(t);

    // ------------------------------------------------ first run
    final fields = find.byType(TextField);
    await t.enterText(fields.at(0), 'correct horse battery staple');
    await t.enterText(fields.at(1), 'correct horse battery staple');
    if (phone) await shot(t, 'first-run');
    await t.tap(find.text('Create vault'));
    await until(t, () => state.unlocked, () => 'vault to unlock');
    await settle(t, 2500); // the "biometric unlock unavailable" note goes

    // ------------------------------------------------ demo vault
    final server1 = host('server1', sshHost, 'demo', tags: ['web'], os: 'ubuntu');
    final server2 = host('server2', sshHost, 'ops', tags: ['web'], os: 'ubuntu');
    final gateway = host('gateway', '203.0.113.10', 'demo', tags: ['edge']);
    final nas = host('nas', '198.51.100.10', 'demo', tags: ['storage'], via: [gateway.id], cert: true, keep: true);
    final pi = host('pi', '198.51.100.50', 'pi', tags: ['homelab'], os: 'raspbian');
    for (final h in [gateway, nas, pi, server1, server2]) {
      await state.core.upsertHost(host: h);
    }
    for (final h in [server1, server2]) {
      await state.saveHostPassword(h, Uint8List.fromList(utf8.encode(demoPassword)));
    }
    await state.core.upsertSnippet(id: AppState.newId(), name: 'Disk space', body: 'df -h /');
    await state.core.upsertSnippet(id: AppState.newId(), name: 'Follow the system log', body: 'journalctl -f');
    state.deviceName = phone ? 'Phone' : 'Desktop PC';
    await state.save(syncAfter: false);

    // Sync with the demo server, plus two more devices in its list.
    await state.sync.createVault(url: syncUrl, serverToken: syncToken, name: 'home');
    final realId = state.meta['deviceId'];
    final realName = state.deviceName;
    for (final (id, name) in [('0f3c8a1e-demo-laptop', 'Laptop'), ('7b21d9c4-demo-server', 'server1 (command line)')]) {
      state.meta['deviceId'] = id;
      state.deviceName = name;
      await state.sync.syncNow();
    }
    state.meta['deviceId'] = realId;
    state.deviceName = realName;
    await state.sync.syncNow();
    try {
      await state.sync.renewCertificate(force: true);
    } catch (e) {
      // ignore: avoid_print
      print('certificate: $e');
    }
    state.refresh();
    await settle(t);

    // ------------------------------------------------ vault and hosts
    if (phone) await shot(t, 'vault');
    await push(t, HostsScreen(state: state));
    await shot(t, 'hosts');
    if (phone) {
      await push(t, HostEditScreen(state: state, host: nas));
      await shot(t, 'host-edit');
      await t.scrollUntilVisible(find.text('Sign in with MyTerm certificate'), 200,
          scrollable: find.byType(Scrollable).last);
      await shot(t, 'host-edit-options');
    }
    await home(t);

    // ------------------------------------------------ devices (real server)
    await push(t, DevicesScreen(state: state));
    await waitFor(t, find.textContaining('Laptop'));
    await shot(t, 'devices');
    await home(t);

    // ------------------------------------------------ terminal
    final s1 = await connect(t, state, server1, hostKeyShot: phone ? 'host-key' : null);
    // Some history for autocomplete. The Windows CI SSH server runs Git Bash.
    final commands = phone
        ? ['systemctl status ssh --no-pager | head -4', 'clear', 'uptime', 'df -h /', 'free -h', 'ls /etc/ssh']
        : ['date', 'git --version', 'clear', 'whoami', 'ls /'];
    for (final c in commands) {
      await run(t, s1, c);
    }
    if (phone) {
      type(s1, 'sy');
      await settle(t, 1500);
      await shot(t, 'terminal');
      type(s1, '\x15'); // Ctrl-U: clear the typed line
      await settle(t, 600);
    } else {
      // A second host, side by side.
      // Same SSH server as server1 here, so its host key is already trusted.
      final s2 = state.openSession(server2);
      await until(t, () => s2.isLive || find.text('Trust').evaluate().isNotEmpty,
          () => 'session to server2 (status: ${s2.status})', seconds: 90);
      if (!s2.isLive) await t.tap(find.text('Trust'));
      await until(t, () => s2.isLive, () => 'session to server2 (status: ${s2.status})', seconds: 90);
      await t.tap(find.byTooltip('Split: side by side'));
      await settle(t, 1500);
      await run(t, s2, 'whoami');
      await run(t, s2, 'date');
      await run(t, s2, 'git --version');
      type(s2, 'df');
      await settle(t, 1500);
      await shot(t, 'split');
    }
    await home(t);

    // ------------------------------------------------ settings
    // Shown with an example address instead of the CI server's.
    state.sync.stop();
    state.meta['syncUrl'] = shownSyncUrl;
    state.refresh();
    await t.tap(find.byIcon(Icons.settings));
    await settle(t);
    await shot(t, 'settings');
  }, timeout: const Timeout(Duration(minutes: 10)));
}
