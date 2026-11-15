import 'package:flutter/material.dart';

import 'src/app_state.dart';
import 'src/screens/home_shell.dart';
import 'src/screens/unlock_screen.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final state = AppState();
  await state.init();
  runApp(MyTermApp(state: state));
}

const navy = Color(0xFF1E2133);
const card = Color(0xFF292C3F);
const accent = Color(0xFF2196F3);

class MyTermApp extends StatefulWidget {
  const MyTermApp({super.key, required this.state});
  final AppState state;

  @override
  State<MyTermApp> createState() => _MyTermAppState();
}

class _MyTermAppState extends State<MyTermApp> with WidgetsBindingObserver {
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState s) {
    // Save on background; auto-lock when backgrounded with no live sessions.
    // Phones go to `paused` in the background; desktop windows to `hidden` when minimised.
    if ((s == AppLifecycleState.paused || s == AppLifecycleState.hidden) && widget.state.unlocked) {
      if (widget.state.liveCount == 0) {
        widget.state.lock();
      } else {
        widget.state.save();
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = ColorScheme.fromSeed(seedColor: accent, brightness: Brightness.dark).copyWith(
      surface: navy,
      surfaceContainer: card,
      primary: accent,
    );
    return MaterialApp(
      title: 'MyTerm',
      navigatorKey: AppState.navigatorKey,
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        colorScheme: scheme,
        scaffoldBackgroundColor: navy,
        cardTheme: const CardThemeData(color: card, margin: EdgeInsets.symmetric(horizontal: 16, vertical: 4)),
        useMaterial3: true,
      ),
      home: ListenableBuilder(
        listenable: widget.state,
        builder: (context, _) =>
            widget.state.unlocked ? HomeShell(state: widget.state) : UnlockScreen(state: widget.state),
      ),
    );
  }
}
