import 'dart:io';

import 'package:flutter/material.dart';

import 'ui/nexdesk_theme.dart';

import 'connection/target_validator.dart';
import 'home/home_screen.dart';
import 'lan/native_desktop.dart';
import 'lan/session_screen.dart';
import 'lan/session_window.dart';
import 'mobile/mobile_home.dart';
import 'preview/native_preview.dart';

void main(List<String> arguments) {
  WidgetsFlutterBinding.ensureInitialized();
  if (Platform.isAndroid || Platform.isIOS) {
    runApp(const NexDeskApp(validator: null, mobile: true));
    return;
  }
  final desktop = NativeDesktop.tryLoad(NativePreview.tryLoad());
  if (arguments.length == 2 &&
      arguments.first == SessionWindowLaunch.argument) {
    final launch = SessionWindowLaunch.decode(arguments.last);
    if (desktop == null) {
      SessionWindow.onCloseRequested(() async {
        await SessionWindow.close();
      });
    }
    runApp(
      _NexDeskMaterial(
        child: desktop == null
            ? Scaffold(
                body: Center(
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      const Text('远控核心不可用。'),
                      TextButton(
                        onPressed: SessionWindow.close,
                        child: const Text('关闭窗口'),
                      ),
                    ],
                  ),
                ),
              )
            : LanSessionScreen(native: desktop, launch: launch),
      ),
    );
    return;
  }
  runApp(
    NexDeskApp(validator: RustTargetValidator.tryLoad(), desktop: desktop),
  );
}

class NexDeskApp extends StatelessWidget {
  const NexDeskApp({
    super.key,
    required this.validator,
    this.desktop,
    this.mobile = false,
  });

  final TargetValidator? validator;
  final NativeDesktop? desktop;
  final bool mobile;

  @override
  Widget build(BuildContext context) => _NexDeskMaterial(
    child: mobile
        ? const MobileHome()
        : HomeScreen(validator: validator, desktop: desktop),
  );
}

class _NexDeskMaterial extends StatelessWidget {
  const _NexDeskMaterial({required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) => MaterialApp(
    title: 'NexDesk',
    debugShowCheckedModeBanner: false,
    theme: nexdeskTheme(Brightness.light),
    darkTheme: nexdeskTheme(Brightness.dark),
    home: child,
  );
}
