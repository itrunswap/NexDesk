import 'dart:io';

import 'package:flutter/material.dart';

import 'ui/nexdesk_theme.dart';

import 'connection/target_validator.dart';
import 'home/home_screen.dart';
import 'lan/native_desktop.dart';
import 'mobile/mobile_home.dart';
import 'preview/native_preview.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  if (Platform.isAndroid || Platform.isIOS) {
    runApp(const NexDeskApp(validator: null, mobile: true));
    return;
  }
  final preview = NativePreview.tryLoad();
  runApp(
    NexDeskApp(
      validator: RustTargetValidator.tryLoad(),
      desktop: NativeDesktop.tryLoad(preview),
    ),
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
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'NexDesk',
      debugShowCheckedModeBanner: false,
      theme: nexdeskTheme(Brightness.light),
      darkTheme: nexdeskTheme(Brightness.dark),
      home: mobile
          ? const MobileHome()
          : HomeScreen(validator: validator, desktop: desktop),
    );
  }
}
